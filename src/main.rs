mod database;

use database::{DbPool, init_database};

mod config;
use config::{Config, get_config};

use std::{
    error::Error,
    time::Duration,
    sync::Arc,
    collections::{VecDeque, HashMap},
    fs,
    path::Path
};

use moka::future::Cache;
use tera::Tera;
use lazy_static::lazy_static;
use dotenv::dotenv;
use chrono::Utc;
use actix_web::{main, web, App, HttpResponse, HttpServer, Result, Responder};


lazy_static! {
    static ref TEMPLATES: Tera = {
        let mut tera = match Tera::new("templates/**/*") {
            Ok(t) => t,
            Err(e) => {
                println!("Parsing error(s): {:?}", e);
                ::std::process::exit(1);
            }
        };
        tera.autoescape_on(vec!["html"]);
        tera
    };

    static ref FAVICON_ICO: Option<Vec<u8>> = {
        let favicon_path = Path::new("favicons/favicon.ico");
        fs::read(favicon_path).ok()
    };

    static ref FAVICONS: HashMap<String, Vec<u8>> = {
        let mut icons = HashMap::new();
        let favicon_dir = Path::new("favicons");
        
        if favicon_dir.exists() {
            match fs::read_dir(favicon_dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_file() {
                                let file_name = entry.file_name().to_string_lossy().to_string();
                                if file_name.ends_with(".webp") {
                                    if let Ok(content) = fs::read(entry.path()) {
                                        icons.insert(file_name, content);
                                    }
                                }
                            }
                        }
                    }
                },
                Err(e) => eprintln!("Error reading favicon directory: {}", e)
            }
        }
        icons
    };

    static ref ROBOTS_TXT: &'static str = "User-agent: *\nAllow: /\n";
}

pub fn calculate_uptime(data: &VecDeque<u32>, window: usize) -> f64 {
    if data.is_empty() {
        return 100.0;
    }
    
    let actual_window = std::cmp::min(window, data.len());
    
    let up_count = data.iter()
        .take(actual_window)
        .filter(|&&x| x > 0)
        .count();
    
    (up_count as f64 / actual_window as f64) * 100.0
}

async fn index_service(data: web::Data<AppState>) -> Result<HttpResponse> {
    if let Some(cached_response) = data.index_cache.get("index").await {
        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .insert_header(("Cache-Control", "public, max-age=30"))
            .body(cached_response));
    }

    let (services, incidents) = tokio::join!(
        data.db.list_services(),
        data.db.list_incidents(false)
    );
    let services = services.unwrap_or_default();
    let incidents = incidents.unwrap_or_default();

    let overall_status = match services.iter().filter(|s| !s.is_online).count() {
        0 => "up",
        n if n == services.len() => "down",
        _ => "partial"
    };

    let service_count = services.len() as f64;
    let mut processed_services = Vec::with_capacity(services.len());
    let mut total_uptimes = [0.0; 4];

    for service in &services {
        let response_times: VecDeque<u32> = service.response_times
            .iter()
            .map(|&x| if x > 0 { x as u32 } else { 0 })
            .collect();

        total_uptimes[0] += calculate_uptime(&response_times, 1440);
        total_uptimes[1] += calculate_uptime(&response_times, 10080);
        total_uptimes[2] += calculate_uptime(&response_times, 43200);
        total_uptimes[3] += calculate_uptime(&response_times, 129600);

        let uptime_24h = calculate_uptime(&response_times, 1440);
        let total_points = response_times.len();
        let points_per_day = 1440;
        let available_days = (total_points as f64 / points_per_day as f64).ceil() as usize;
        let max_days = 90;

        let mut status_segments = Vec::with_capacity(max_days);

        if available_days < max_days {
            status_segments.extend(std::iter::repeat(serde_json::json!({
                "name": "no-data",
                "description": "No data"
            })).take(max_days - available_days));
        }

        for day in 0..available_days.min(max_days) {
            let start_idx = day * points_per_day;
            if start_idx >= total_points {
                break;
            }

            let end_idx = std::cmp::min((day + 1) * points_per_day, total_points);
            let day_data: Vec<_> = response_times.iter()
                .skip(start_idx)
                .take(end_idx - start_idx)
                .cloned()
                .collect();

            if !day_data.is_empty() {
                let down_count = day_data.iter().filter(|&&x| x == 0).count();
                let down_percentage = (down_count as f64 / day_data.len() as f64) * 100.0;

                let (status, description) = if down_percentage > 50.0 {
                    ("down", format!("{:.1}%", 100.0 - down_percentage))
                } else if down_percentage > 25.0 {
                    ("partial", format!("{:.1}%", 100.0 - down_percentage))
                } else {
                    ("up", format!("{:.1}%", 100.0 - down_percentage))
                };

                status_segments.push(serde_json::json!({
                    "name": status,
                    "description": description
                }));
            } else {
                status_segments.push(serde_json::json!({
                    "name": "no-data",
                    "description": "No data"
                }));
            }
        }

        processed_services.push(serde_json::json!({
            "id": service.id,
            "name": service.name,
            "uptime": format!("{:.2}", uptime_24h),
            "status": status_segments
        }));
    }

    let [avg_uptime_24h, avg_uptime_7d, avg_uptime_30d, avg_uptime_90d] = if service_count > 0.0 {
        total_uptimes.map(|total| total / service_count)
    } else {
        [100.0; 4]
    };

    let formatted_incidents: Vec<_> = incidents.into_iter().map(|incident| {
        serde_json::json!({
            "service_name": incident.service_name,
            "start_time": incident.start_time.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            "description": incident.description
        })
    }).collect();

    let mut ctx = tera::Context::new();
    ctx.insert("status", &overall_status);
    ctx.insert("services", &processed_services);
    ctx.insert("situations", &formatted_incidents);
    ctx.insert("uptime_24h", &format!("{:.2}", avg_uptime_24h));
    ctx.insert("uptime_7d", &format!("{:.2}", avg_uptime_7d));
    ctx.insert("uptime_30d", &format!("{:.2}", avg_uptime_30d));
    ctx.insert("uptime_90d", &format!("{:.2}", avg_uptime_90d));
    ctx.insert("load_time", &Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string());

    let rendered = TEMPLATES.render("index.html", &ctx)
        .map_err(|e| {
            eprintln!("Template error: {}", e);
            if let Some(source) = Error::source(&e) {
                eprintln!("Caused by: {}", source);
            }
            actix_web::error::ErrorInternalServerError("Template error")
        })?;

    data.index_cache.insert("index".to_string(), rendered.clone()).await;

    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .insert_header(("Cache-Control", "public, max-age=30"))
        .body(rendered))
}

async fn service_detail_service(path: web::Path<String>, data: web::Data<AppState>) -> Result<HttpResponse> {
    let service_id = path.into_inner();
    
    if let Some(cached_response) = data.service_cache.get(&service_id).await {
        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .insert_header(("Cache-Control", "public, max-age=30"))
            .body(cached_response));
    }

    let services = data.db.list_services().await.unwrap_or_default();
    let service = match services.iter().find(|s| s.id == service_id) {
        Some(service) => service,
        None => return Ok(HttpResponse::NotFound().finish())
    };

    let response_times: VecDeque<u32> = service.response_times
        .iter()
        .map(|&x| if x > 0 { x as u32 } else { 0 })
        .collect();

    let uptimes = [1440, 10080, 43200, 129600]
        .map(|window| calculate_uptime(&response_times, window));
    
    let total_points = response_times.len();
    let points_per_day = 1440;
    let available_days = (total_points as f64 / points_per_day as f64).ceil() as usize;
    let max_days = 90;
    
    let mut status_segments = Vec::with_capacity(max_days);
    
    if available_days < max_days {
        status_segments.extend(std::iter::repeat(serde_json::json!({
            "name": "no-data",
            "description": "No data"
        })).take(max_days - available_days));
    }

    for day in 0..available_days.min(max_days) {
        let start_idx = day * points_per_day;
        if start_idx >= total_points {
            break;
        }

        let end_idx = std::cmp::min((day + 1) * points_per_day, total_points);
        let day_data: Vec<_> = response_times.iter()
            .skip(start_idx)
            .take(end_idx - start_idx)
            .cloned()
            .collect();

        if !day_data.is_empty() {
            let down_count = day_data.iter().filter(|&&x| x == 0).count();
            let down_percentage = (down_count as f64 / day_data.len() as f64) * 100.0;

            let (status, description) = if down_percentage > 50.0 {
                ("down", format!("{:.1}%", 100.0 - down_percentage))
            } else if down_percentage > 25.0 {
                ("partial", format!("{:.1}%", 100.0 - down_percentage))
            } else {
                ("up", format!("{:.1}%", 100.0 - down_percentage))
            };

            status_segments.push(serde_json::json!({
                "name": status,
                "description": description
            }));
        } else {
            status_segments.push(serde_json::json!({
                "name": "no-data",
                "description": "No data"
            }));
        }
    }

    let last_30_days: Vec<u32> = response_times.iter()
        .take(43200)
        .copied()
        .collect();

    let days = (last_30_days.len() + points_per_day - 1) / points_per_day;
    let daily_averages: Vec<u32> = (0..days)
        .filter_map(|day| {
            let start = day * points_per_day;
            let end = std::cmp::min((day + 1) * points_per_day, last_30_days.len());
            let day_data = &last_30_days[start..end];
            
            if !day_data.is_empty() {
                let (sum, count) = day_data.iter()
                    .filter(|&&x| x > 0)
                    .fold((0u64, 0usize), |(sum, count), &x| (sum + x as u64, count + 1));
                if count > 0 {
                    Some((sum / count as u64) as u32)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    let (max_daily_avg, min_daily_avg) = if !daily_averages.is_empty() {
        let max = daily_averages.iter().copied().max().unwrap_or(0) + 50;
        let min = daily_averages.iter()
            .copied()
            .filter(|&x| x > 0)
            .min()
            .map(|x| x.saturating_sub(50))
            .unwrap_or(0);
        (max, min)
    } else {
        (0, 0)
    };

    let (max_response_time, min_response_time, avg_response_time) = {
        let max = last_30_days.iter().copied().max().unwrap_or(0);
        let min = last_30_days.iter().filter(|&&x| x > 0).min().copied().unwrap_or(0);
        let avg = if !daily_averages.is_empty() {
            let sum: u64 = daily_averages.iter().map(|&x| x as u64).sum();
            (sum / daily_averages.len() as u64) as u32
        } else {
            0
        };
        (max, min, avg)
    };

    let graph_points = {
        let valid_points: Vec<u32> = last_30_days.iter()
            .filter(|&&x| x > 0)
            .copied()
            .collect();

        let points = if valid_points.len() <= 30 {
            valid_points
        } else {
            let step = valid_points.len() / 30;
            (0..30)
                .filter_map(|i| {
                    let start = i * step;
                    let end = start + step;
                    let slice = &valid_points[start..std::cmp::min(end, valid_points.len())];
                    if !slice.is_empty() {
                        let sum: u64 = slice.iter().map(|&x| x as u64).sum();
                        Some((sum / slice.len() as u64) as u32)
                    } else {
                        None
                    }
                })
                .collect()
        };

        let range = max_daily_avg.saturating_sub(min_daily_avg);
        points.iter().enumerate()
            .map(|(i, &value)| {
                let percentage = if value > min_daily_avg && range > 0 {
                    ((value - min_daily_avg) as f64 / range as f64 * 80.0).min(80.0)
                } else {
                    0.0
                };
                serde_json::json!({
                    "left": format!("{}%", 2.5 + (i as f64 / (points.len() - 1).max(1) as f64) * 95.0),
                    "bottom": format!("{}%", percentage),
                    "value": value
                })
            })
            .collect::<Vec<_>>()
    };

    let range = max_daily_avg - min_daily_avg;
    let step_size = range / 4;
    let y_axis_steps: Vec<u32> = (0..=4)
        .map(|i| min_daily_avg + (step_size * i))
        .collect();

    let mut ctx = tera::Context::new();
    ctx.insert("service", &service);
    ctx.insert("status", &status_segments);
    ctx.insert("uptime_24h", &format!("{:.2}", uptimes[0]));
    ctx.insert("uptime_7d", &format!("{:.2}", uptimes[1]));
    ctx.insert("uptime_30d", &format!("{:.2}", uptimes[2]));
    ctx.insert("uptime_90d", &format!("{:.2}", uptimes[3]));
    ctx.insert("graph_points", &graph_points);
    ctx.insert("y_axis_steps", &y_axis_steps);
    ctx.insert("avg_response_time", &avg_response_time);
    ctx.insert("max_response_time", &max_response_time);
    ctx.insert("min_response_time", &min_response_time);
    ctx.insert("load_time", &Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string());

    let rendered = TEMPLATES.render("service.html", &ctx)
        .map_err(|e| {
            eprintln!("Template error: {}", e);
            if let Some(source) = e.source() {
                eprintln!("Caused by: {}", source);
            }
            actix_web::error::ErrorInternalServerError("Template error")
        })?;

    data.service_cache.insert(service_id, rendered.clone()).await;

    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .insert_header(("Cache-Control", "public, max-age=30"))
        .body(rendered))
}

#[derive(Clone)]
struct CachedResponse {
    content: Vec<u8>,
    content_type: String,
}

async fn favicon_service(data: web::Data<AppState>) -> Result<HttpResponse> {
    if let Some(cached) = data.static_cache.get("favicon.ico").await {
        return Ok(HttpResponse::Ok()
            .content_type(cached.content_type.as_str())
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .body(cached.content));
    }

    if let Some(content) = FAVICON_ICO.as_ref() {
        let cached = CachedResponse {
            content: content.clone(),
            content_type: "image/x-icon".to_string(),
        };
        data.static_cache.insert("favicon.ico".to_string(), cached.clone()).await;
        
        Ok(HttpResponse::Ok()
            .content_type("image/x-icon")
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .body(content.clone()))
    } else {
        Ok(HttpResponse::NotFound().finish())
    }
}

async fn favicons_service(path: web::Path<String>, data: web::Data<AppState>) -> Result<HttpResponse> {
    let favicon_name = path.into_inner();
    
    if let Some(cached) = data.static_cache.get(&favicon_name).await {
        return Ok(HttpResponse::Ok()
            .content_type(cached.content_type.as_str())
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .body(cached.content));
    }

    if let Some(content) = FAVICONS.get(&favicon_name) {
        let cached = CachedResponse {
            content: content.clone(),
            content_type: "image/webp".to_string(),
        };
        data.static_cache.insert(favicon_name, cached.clone()).await;
        
        Ok(HttpResponse::Ok()
            .content_type("image/webp")
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .body(content.clone()))
    } else {
        Ok(HttpResponse::NotFound().finish())
    }
}

async fn robots_service(data: web::Data<AppState>) -> Result<HttpResponse> {
    if let Some(cached) = data.static_cache.get("robots.txt").await {
        return Ok(HttpResponse::Ok()
            .content_type(cached.content_type.as_str())
            .insert_header(("Cache-Control", "public, max-age=86400"))
            .body(cached.content));
    }

    let cached = CachedResponse {
        content: ROBOTS_TXT.as_bytes().to_vec(),
        content_type: "text/plain".to_string(),
    };
    data.static_cache.insert("robots.txt".to_string(), cached.clone()).await;

    Ok(HttpResponse::Ok()
        .content_type("text/plain")
        .insert_header(("Cache-Control", "public, max-age=86400"))
        .body(*ROBOTS_TXT))
}

async fn ping_service() -> impl Responder {
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .body("pong")
}

#[derive(Clone)]
struct AppState {
    db: Arc<DbPool>,
    index_cache: Arc<Cache<String, String>>,
    service_cache: Arc<Cache<String, String>>,
    static_cache: Arc<Cache<String, CachedResponse>>,
}

#[main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();

    let config = get_config().expect("Failed to load configuration");

    let db_pool = DbPool::new(
        config.database_host.clone(),
        config.database_port,
        config.database_name.clone(),
        config.database_user.clone(),
        config.database_password.clone()
    ).await.expect("Failed to create database pool");
    init_database(&db_pool).await.expect("Failed to initialize database");

    let index_cache = Cache::builder()
        .time_to_live(Duration::from_secs(30))
        .build();
        
    let service_cache = Cache::builder()
        .time_to_live(Duration::from_secs(30))
        .build();

    let static_cache = Cache::builder()
        .time_to_live(Duration::from_secs(3600))
        .build();
    
    let app_state = AppState {
        db: Arc::new(db_pool),
        index_cache: Arc::new(index_cache),
        service_cache: Arc::new(service_cache),
        static_cache: Arc::new(static_cache),
    };

    run_web_server(app_state, &config).await
}

async fn run_web_server(app_state: AppState, config: &Config) -> std::io::Result<()> {
    HttpServer::new(move || {
        let app_state = app_state.clone();
        App::new()
            .app_data(web::Data::new(app_state))
            .service(web::resource("/").to(index_service))
            .service(web::resource("/service/{service_id}").to(service_detail_service))
            .service(web::resource("/favicons/{filename}").to(favicons_service))
            .service(web::resource("/favicon.ico").to(favicon_service))
            .service(web::resource("/robots.txt").to(robots_service))
            .service(web::resource("/ping").to(ping_service))
    })
    .workers(config.workers as usize)
    .bind(format!("{}:{}", config.host, config.port))?
    .run()
    .await
}