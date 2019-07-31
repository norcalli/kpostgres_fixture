`kpostgres_fixture`

[![docs.rs](https://docs.rs/kpostgres_fixture/badge.svg)](https://docs.rs/kpostgres_fixture)

This provides two main functions:
- `with_temporary_postgres`: Creates a temporary postgres instance using docker.
- `with_temporary_database`: Creates a temporary DATABASE inside an existing postgres instance.

They both pass the parameters and TlsMode so that you can create a connection however you want.

This is useful for things like running migrations in a test database in an isolated environment.

The best example of usage is taken directly from my tests:

```rust
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

        let result = with_temporary_postgres("postgres:11", |params, tls_mode, _| -> Result<()> {
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
            .user("postgres", None)
            .database("postgres")
            .build(params::Host::Tcp("localhost".to_owned()));
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
```

