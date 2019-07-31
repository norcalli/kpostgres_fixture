#![warn(clippy::all)]
use std::time::Duration;

use derive_more::From;
use log::*;
use postgres::params::{self, ConnectParams};
use postgres::{Connection, TlsMode};
use rand::{distributions, thread_rng, Rng};

#[derive(From, Debug)]
pub enum Error {
    #[cfg(feature = "docker")]
    Docker(dockworker::errors::Error),
    #[cfg(feature = "docker")]
    DockerCreationFailed(&'static str),
    Postgres(postgres::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

// TODO allow passing a version via PostgresConfig
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

/// Generate a random string of [a-z0-9]
/// Lowercase so that just in case I forget to quote wrap something, it is
/// still interpreted correctly, since in postgres, identifier A and a are
/// equivalent.
fn random_string(length: usize) -> String {
    let mut rng = thread_rng();
    std::iter::repeat(())
        .map(|()| rng.sample(distributions::Alphanumeric).to_ascii_lowercase())
        // .map(|()| rng.sample(distributions::Alphanumeric))
        .take(length)
        .collect()
}

macro_rules! try_ {
    ($e:block) => {
        (|| Ok($e))()
    };
}

/// Methodology taken from http://wiki.postgresql.org/wiki/Shared_Database_Hosting
pub fn with_temporary_database<T, F: FnOnce(ConnectParams, TlsMode) -> T>(
    params: ConnectParams,
    tls_mode: TlsMode,
    f: F,
) -> Result<T> {
    let dbname = format!("kpg_fixture_{}", random_string(20));
    // I can skip escaping this since the value is alphanumeric
    let dbmainuserpass = random_string(32);

    debug!(
        "Creating database {:?} with password {:?} and default user {:?}",
        dbname, dbmainuserpass, dbname
    );
    let new_params = {
        let mut new_params = ConnectParams::builder();
        new_params
            .port(params.port())
            .user(&dbname, Some(&dbmainuserpass))
            .database(&dbname)
            .connect_timeout(params.connect_timeout());
        for (key, value) in params.options() {
            new_params.option(key, value);
        }
        // new_params.option("AUTOCOMMIT", "ON");
        new_params.build(params.host().clone())
    };

    let conn = Connection::connect(params, clone_tls_mode(&tls_mode))?;

    // Setup a new user
    // These must be executed separately since CREATE/DROP DATABASE cannot be executed inside a
    // transaction and multi-statement queries are implicitly wrapped in a transaction.
    // Ref: https://www.postgresql.org/docs/current/protocol-flow.html#PROTOCOL-FLOW-MULTI-STATEMENT
    debug!("Setting up database");
    conn.batch_execute(&format!(
        "CREATE ROLE {dbname:?}
            NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT
            LOGIN ENCRYPTED PASSWORD '{dbmainuserpass}';",
        // Interpolating like this is safe since I use an Alphanumeric distribution
        dbname = dbname,
        dbmainuserpass = dbmainuserpass
    ))?;
    // Try block this so I can rollback incrementally.
    let result = try_!({
        conn.batch_execute(&format!(
            "CREATE DATABASE {dbname:?} WITH OWNER={dbname:?};",
            dbname = dbname
        ))?;
        let result: Result<T> = try_!({
            conn.batch_execute(&format!(
                "REVOKE ALL ON DATABASE {dbname:?} FROM public;",
                dbname = dbname
            ))?;
            debug!("Finished setting up database");

            f(new_params, tls_mode)
        });
        debug!("Starting cleanup");
        conn.batch_execute(&format!("DROP DATABASE {dbname:?};", dbname = dbname))?;
        result?
    });
    conn.batch_execute(&format!("DROP ROLE {dbname:?};", dbname = dbname))?;
    debug!("Finished cleanup");
    result
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

    use std::sync::{Once, ONCE_INIT};

    static INIT: Once = ONCE_INIT;

    #[cfg(feature = "docker")]
    #[test]
    fn temp_pg() {
        INIT.call_once(|| {
            env_logger::init();
        });

        let result = with_temporary_postgres(|params, tls_mode, _| -> Result<()> {
            // let conn = Connection::connect(params.clone(), clone_tls_mode(&tls_mode))?;
            with_temporary_database(params, tls_mode, |params, tls_mode| -> Result<()> {
                let conn = Connection::connect(params, tls_mode)?;
                conn.batch_execute("CREATE TABLE test()")?;
                conn.execute("TABLE test", &[])?;
                Ok(())
            })?
        })
        .expect("Failed to create temporary database");
        println!("{:#?}", result);
        result.expect("Inner result failed");
    }

    #[test]
    fn temp_db() {
        INIT.call_once(|| {
            env_logger::init();
        });

        let connect_params = ConnectParams::builder()
            .port(5432)
            // .user("postgres", Some("postgres"))
            .user("postgres", None)
            .database("postgres")
            .build(params::Host::Tcp("localhost".to_owned()));
        // let conn = Connection::connect(params.clone(), clone_tls_mode(&tls_mode))?;
        let result = with_temporary_database(
            connect_params,
            TlsMode::None,
            |params, tls_mode| -> Result<()> {
                let conn = Connection::connect(params, tls_mode)?;
                conn.batch_execute("CREATE TABLE test()")?;
                conn.execute("TABLE test", &[])?;
                Ok(())
            },
        )
        .expect("Failed to create temporary database");
        println!("{:#?}", result);
        result.expect("Inner result failed");
    }
}
