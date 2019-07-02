use std::time::Duration;

use derive_more::From;
use postgres::params::{self, ConnectParams};
use postgres::{Connection, TlsMode};

#[derive(From, Debug)]
pub enum Error {
    #[cfg(feature = "docker")]
    Docker(dockworker::errors::Error),
    #[cfg(feature = "docker")]
    DockerCreationFailed(&'static str),
    Postgres(postgres::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(feature = "docker")]
pub fn with_temporary_postgres<T, F: FnOnce(ConnectParams, TlsMode, Connection) -> T>(
    f: F,
) -> Result<T> {
    use std::borrow::Borrow;
    let docker = dockworker::Docker::connect_with_defaults()?;

    let mut container_host_config = dockworker::ContainerHostConfig::new();
    container_host_config.publish_all_ports(true);
    let container_id = docker
        .create_container(
            None,
            dockworker::ContainerCreateOptions::new("postgres:11")
                .host_config(container_host_config),
        )?
        .id;

    let result = (|| -> Result<T> {
        docker.start_container(&container_id)?;

        let docker = docker.borrow();
        let result = (|| -> Result<T> {
            let mut filters = dockworker::container::ContainerFilters::new();
            filters.id(&container_id);
            let container = docker.list_containers(None, None, None, filters)?;

            let container = container.first().unwrap();

            let postgres_port = dbg!(&container.Ports)
                .iter()
                .filter(|p| p.PrivatePort == 5432)
                .flat_map(|p| p.PublicPort)
                .next()
                .ok_or_else(|| Error::DockerCreationFailed("Failed to find postgres port"))?;

            let connect_params = ConnectParams::builder()
                .port(dbg!(postgres_port as u16))
                // .user("postgres", Some("postgres"))
                .user("postgres", None)
                .database("postgres")
                .build(params::Host::Tcp("localhost".to_owned()));

            let tls_mode = TlsMode::None;

            let mut n = 0;
            let connection = loop {
                n += 1;
                match Connection::connect(connect_params.clone(), clone_tls_mode(&tls_mode)) {
                    Ok(conn) => break Ok(conn),
                    // TODO timeouterror
                    Err(err) => {
                        if n >= 100 {
                            break Err(err);
                        }
                    }
                }

                std::thread::sleep(Duration::from_millis(100));
            };
            // drop(connection);
            // Ok(f(connect_params, tls_mode))
            Ok(f(connect_params, tls_mode, connection?))
        })();
        docker.stop_container(&container_id, std::time::Duration::from_secs(5))?;
        Ok(result?)
    })();
    docker.remove_container(&container_id, None, Some(true), None)?;
    Ok(result?)
}

pub fn clone_tls_mode<'a>(tls_mode: &TlsMode<'a>) -> TlsMode<'a> {
    match tls_mode {
        TlsMode::None => TlsMode::None,
        TlsMode::Prefer(ref handshake) => TlsMode::Prefer(*handshake),
        TlsMode::Require(ref handshake) => TlsMode::Require(*handshake),
    }
}

/// Methodology taken from http://wiki.postgresql.org/wiki/Shared_Database_Hosting
pub fn with_temporary_database<T, F: FnOnce(ConnectParams, TlsMode) -> T>(
    params: ConnectParams,
    tls_mode: TlsMode,
    f: F,
) -> Result<T> {
    // TODO generate random name
    let dbname = "asdasldkjflskf";
    let dbmainuser = dbname;
    // TODO properly escape this
    let dbmainuserpass = "ASLDKFJALSKDJ";

    let new_params = {
        let mut new_params = ConnectParams::builder();
        new_params
            .port(params.port())
            .user(dbmainuser, Some(dbmainuserpass))
            .database(dbname)
            .connect_timeout(params.connect_timeout());
        for (key, value) in params.options() {
            new_params.option(key, value);
        }
        // new_params.option("AUTOCOMMIT", "ON");
        new_params.build(params.host().clone())
    };

    let conn = Connection::connect(params, clone_tls_mode(&tls_mode))?;

    /* TODO
     * thread 'tests::it_works' panicked at 'called `Result::unwrap()` on an `Err` value: Postgres(Error(Db(DbError { severity: "ERROR", parsed_severity: Some(Error), code: SqlState("25001"), message: "CREATE DATABASE cannot run inside a transaction block", detail: None, hint: None, position: None, where_: None, schema: None, table: None, column: None, datatype: None, constraint: None, file: Some("xact.c"), line: Some(3213), routine: Some("PreventInTransactionBlock") })))', src/libcore/result.rs:997:5
     * note: Run with `RUST_BACKTRACE=1` environment variable to display a backtrace.
     */
    // Setup a new user
    conn.batch_execute(&format!(r#"
CREATE ROLE {dbname} NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT LOGIN ENCRYPTED PASSWORD '{dbmainuserpass}';
CREATE DATABASE {dbname} WITH OWNER={dbname};
REVOKE ALL ON DATABASE {dbname} FROM public;
     "#, dbname=dbname, dbmainuserpass=dbmainuserpass))?;

    let result = f(new_params, tls_mode);

    // Cleanup
    conn.batch_execute(&format!(
        r#"
DROP DATABASE {dbname};
DROP ROLE {dbname};
    "#,
        dbname = dbname,
    ))?;
    // let conn = conn.execute(format!("SET SESSION AUTHORIZATION {}", dbmainuser), &[])?;
    Ok(result)
}

// /// Methodology taken from http://wiki.postgresql.org/wiki/Shared_Database_Hosting
// pub fn with_temporary_database_conn<T, F: FnOnce(Connection) -> T>(
//     params: ConnectParams,
//     tls_mode: TlsMode,
//     f: F,
// ) -> Result<T> {
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "docker")]
    #[test]
    fn it_works() {
        let result = with_temporary_postgres(|params, tls_mode, _| -> Result<()> {
            // let conn = Connection::connect(params.clone(), clone_tls_mode(&tls_mode))?;
            let result =
                with_temporary_database(params, tls_mode, |params, tls_mode| -> Result<()> {
                    let conn = Connection::connect(params, tls_mode)?;
                    conn.batch_execute("CREATE TABLE test")?;
                    conn.execute("TABLE FROM test", &[])?;
                    Ok(())
                })?;
            result
        })
        .unwrap();
        result.unwrap();
        assert_eq!(2 + 2, 4);
    }
}
