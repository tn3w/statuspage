use chrono::{DateTime, Utc};
use std::{
    fmt,
    sync::Arc,
    error::Error,
    io::Error as IoError
};

use tokio_postgres::NoTls as AsyncNoTls;
use deadpool_postgres::{Config, Pool, Runtime};
use serde::Serialize;


pub async fn init_database(pool: &DbPool) -> Result<(), MonitoringError> {
    let client = pool.pool.get().await
        .map_err(|e| MonitoringError(e.to_string()))?;
    
    client.batch_execute("
        CREATE TABLE IF NOT EXISTS services (
            id VARCHAR(255) PRIMARY KEY,
            name VARCHAR(255) NOT NULL,
            server_url TEXT NOT NULL,
            response_times INTEGER[] DEFAULT array[]::INTEGER[],
            is_online BOOLEAN DEFAULT false
        );

        CREATE TABLE IF NOT EXISTS incidents (
            id SERIAL PRIMARY KEY,
            service_id VARCHAR(255) REFERENCES services(id),
            service_name VARCHAR(255) NOT NULL,
            start_time TIMESTAMP WITH TIME ZONE NOT NULL,
            end_time TIMESTAMP WITH TIME ZONE,
            description TEXT NOT NULL
        );
    ").await.map_err(|e| MonitoringError(e.to_string()))?;

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub server_url: String,
    pub response_times: Vec<i32>,
    pub is_online: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Incident {
    pub id: i32,
    pub service_id: String,
    pub service_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct MonitoringError(pub String);

impl fmt::Display for MonitoringError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for MonitoringError {}

impl From<Box<dyn Error + Send + Sync>> for MonitoringError {
    fn from(err: Box<dyn Error + Send + Sync>) -> Self {
        MonitoringError(err.to_string())
    }
}

impl From<IoError> for MonitoringError {
    fn from(err: IoError) -> MonitoringError {
        MonitoringError(err.to_string())
    }
}

impl From<String> for MonitoringError {
    fn from(err: String) -> MonitoringError {
        MonitoringError(err)
    }
}

#[derive(Clone)]
pub struct DbPool {
    pool: Arc<Pool>,
}

impl DbPool {
    pub async fn new(
        host: String,
        port: u16,
        dbname: String,
        user: String,
        password: String,
    ) -> Result<Self, MonitoringError> {
        let mut cfg = Config::new();
        cfg.host = Some(host);
        cfg.port = Some(port);
        cfg.dbname = Some(dbname);
        cfg.user = Some(user);
        cfg.password = Some(password);

        let pool = cfg.create_pool(Some(Runtime::Tokio1), AsyncNoTls)
            .map_err(|e| MonitoringError(e.to_string()))?;
        Ok(Self { pool: Arc::new(pool) })
    }

    pub async fn list_services(&self) -> Result<Vec<Service>, MonitoringError> {
        let client = self.pool.get().await
            .map_err(|e| MonitoringError(e.to_string()))?;
        let rows = client.query("SELECT id, name, server_url, response_times, is_online FROM services", &[])
            .await.map_err(|e| MonitoringError(e.to_string()))?;

        let services = rows.iter().map(|row| Service {
            id: row.get(0),
            name: row.get(1),
            server_url: row.get(2),
            response_times: row.get(3),
            is_online: row.get(4),
        }).collect();

        Ok(services)
    }

    pub async fn list_incidents(&self, include_closed: bool) -> Result<Vec<Incident>, MonitoringError> {
        let client = self.pool.get().await
            .map_err(|e| MonitoringError(e.to_string()))?;
        
        let query = if include_closed {
            "SELECT id, service_id, service_name, start_time, end_time, description FROM incidents"
        } else {
            "SELECT id, service_id, service_name, start_time, end_time, description FROM incidents WHERE end_time IS NULL"
        };
        
        let rows = client.query(query, &[])
            .await.map_err(|e| MonitoringError(e.to_string()))?;
        
        let incidents = rows.iter().map(|row| {
            let start_time: DateTime<Utc> = row.get(3);
            let end_time: Option<DateTime<Utc>> = row.get(4);
            
            Incident {
                id: row.get(0),
                service_id: row.get(1),
                service_name: row.get(2),
                start_time,
                end_time,
                description: row.get(5),
            }
        }).collect();

        Ok(incidents)
    }
}