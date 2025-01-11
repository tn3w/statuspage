use std::{
    env,
    error::Error
};
use clap::Parser;


#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[arg(short = 'h', long, default_value = "127.0.0.1")]
    host: String,

    #[arg(short = 'p', long, default_value = "8080")]
    port: u16,

    #[arg(short = 'w', long, default_value = "16")]
    workers: u16,

    #[arg(long, default_value = "localhost")]
    database_host: String,

    #[arg(long, default_value = "5432")]
    database_port: u16,

    #[arg(long, default_value = "postgres")]
    database_name: String,

    #[arg(long, default_value = "postgres")]
    database_user: String,

    #[arg(long)]
    database_password: Option<String>,
}

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub workers: u16,
    pub database_host: String,
    pub database_port: u16,
    pub database_name: String,
    pub database_user: String,
    pub database_password: String,
}

pub fn get_config() -> Result<Config, Box<dyn Error>> {
    let args = Args::parse();

    let host = if env::args().any(|arg| arg == "--host" || arg == "-h") {
        args.host
    } else {
        env::var("HOST").unwrap_or(String::from("127.0.0.1"))
    };

    let port = if env::args().any(|arg| arg == "--port" || arg == "-p") {
        args.port
    } else {
        env::var("PORT")
            .map(|p| p.parse::<u16>().expect("PORT must be a valid port number"))
            .unwrap_or(8080)
    };

    let workers = if env::args().any(|arg| arg == "--workers" || arg == "-w") {
        args.workers
    } else {
        env::var("WORKERS")
            .map(|w| w.parse::<u16>().expect("WORKERS must be a valid number"))
            .unwrap_or(16)
    };

    let database_host = if env::args().any(|arg| arg == "--database-host") {
        args.database_host
    } else {
        env::var("DATABASE_HOST").unwrap_or(String::from("localhost"))
    };

    let database_port = if env::args().any(|arg| arg == "--database-port") {
        args.database_port
    } else {
        env::var("DATABASE_PORT")
            .map(|p| p.parse::<u16>().expect("DATABASE_PORT must be a valid port number"))
            .unwrap_or(5432)
    };

    let database_name = if env::args().any(|arg| arg == "--database-name") {
        args.database_name
    } else {
        env::var("DATABASE_NAME").unwrap_or(String::from("postgres"))
    };

    let database_user = if env::args().any(|arg| arg == "--database-user") {
        args.database_user
    } else {
        env::var("DATABASE_USER").unwrap_or(String::from("postgres"))
    };

    let database_password = if env::args().any(|arg| arg == "--database-password") {
        args.database_password
    } else {
        env::var("DATABASE_PASSWORD").ok().or(args.database_password)
    }.expect("DATABASE_PASSWORD must be provided either via environment variable or command line argument");

    Ok(Config {
        host,
        port,
        workers,
        database_host,
        database_port,
        database_name,
        database_user,
        database_password,
    })
}