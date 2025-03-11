use axum::{body::Body, extract::State, http::{header, HeaderValue, Method, Request}, response::{IntoResponse, Response}, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, Level};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tower_http::{cors::{CorsLayer, Any}, trace::TraceLayer };
use tracing_subscriber::FmtSubscriber;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UploadKeyPackagePayload {
    user_id: String,
    key_package: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FetchKeyPackagePayload {
    user_ids: Vec<String>,
}

#[derive(Clone, Default)]
struct AppState {
    key_packages: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

async fn upload_keypackage(
    State(state): State<AppState>,
    Json(payload): Json<UploadKeyPackagePayload>,
)-> impl IntoResponse {
    println!("upload_keypackage: {:?}", payload);
    let mut db = state.key_packages.write().await;
    db.insert(payload.user_id, payload.key_package);
    
    Json({
        serde_json::json!({
            "status": "ok"
        })
    })
}

async fn get_keypackages(
    State(state): State<AppState>,
    Json(payload): Json<FetchKeyPackagePayload>,
) -> Json<HashMap<String, Vec<u8>>> {
    println!("get_key_packages: {:?}", payload);
    let db = state.key_packages.read().await;
    let result = payload.user_ids.iter()
        .filter_map(|id| db.get(id).map(|kp| (id.clone(), kp.clone())))
        .collect();
    Json(result)
}

async fn log_request(request: Request<Body>, next: axum::middleware::Next)  -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let response = next.run(request).await;

    info!("{} {} -> {}", method, uri, response.status());
    response
}

async fn preflight() -> impl IntoResponse {
    Json({
        serde_json::json!({
            "status": "ok"
        })
    })
}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");
    let state = AppState::default();

    let cors = CorsLayer::new()
    .allow_origin(Any) 
    .allow_methods([Method::GET, Method::POST, Method::OPTIONS]) 
    .allow_headers(Any);
    let app = Router::new()
        .route("/health", post(|| async { "OK" }))
        .route("/mls/upload_keypackage", post(upload_keypackage))
        .route("/mls/upload_keypackage", axum::routing::options(preflight))
        .route("/mls/get_keypackages", post(get_keypackages))
        .route("/mls/get_keypackages", axum::routing::options(preflight))
        .layer(cors)
        .layer(axum::middleware::from_fn(log_request))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:5151")
        .await
        .unwrap();
    info!("Listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

