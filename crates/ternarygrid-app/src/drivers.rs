//! Registration of the database drivers compiled into this binary.

use std::sync::Arc;
use ternarygrid_core::registry::DriverRegistry;

/// Register every built-in driver.
pub fn register_all(reg: &mut DriverRegistry) {
    ternarygrid_driver_postgres::register(reg);
    ternarygrid_driver_mysql::register(reg);
    reg.register(Arc::new(ternarygrid_driver_mssql::SqlServerDriver::new()));
    reg.register(Arc::new(ternarygrid_driver_clickhouse::ClickHouseDriver::new()));
    ternarygrid_driver_oracle::register(reg);
    ternarygrid_driver_cassandra::register(reg);
    ternarygrid_driver_redis::register(reg);
    ternarygrid_driver_mongodb::register(reg);
    ternarygrid_driver_elasticsearch::register(reg);
    reg.register(Arc::new(ternarygrid_driver_sqlite::SqliteDriver::new()));
}
