use axum::{
    body::Body,
    extract::State,
    http::{ Method, Request, HeaderMap },
    response::{ IntoResponse, Response, sse::Event, Sse },
    routing::{ post, get },
    Json,
    Router,
};
use serde::{ Deserialize, Serialize };
use tracing::{ info, Level };
use std::{ collections::{ HashMap, HashSet }, sync::Arc };
use tower_http::{ cors::{ CorsLayer, Any }, trace::TraceLayer };
use tracing_subscriber::FmtSubscriber;
use tokio::sync::{ broadcast, mpsc, RwLock };

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UploadKeyPackagePayload {
    user_id: String,
    key_package: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FetchKeyPackagePayload {
    user_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ChatMessage {
    from: String, // sender
    to: Vec<String>, //group id
    mess_type: String, // welcome, encrypt_message, ping
    message: Vec<u8>, // mls encrypt message or string bytes
}
#[derive(Clone, Default)]
struct AppState {
    key_packages: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    users: Arc<RwLock<HashMap<String, mpsc::Sender<ChatMessage>>>>,
    rooms: Arc<RwLock<HashMap<String, HashSet<String>>>>,
}

async fn upload_keypackage(
    State(state): State<AppState>,
    Json(payload): Json<UploadKeyPackagePayload>
) -> impl IntoResponse {
    println!("upload_keypackage: {:?}", payload);
    let mut db = state.key_packages.write().await;
    db.insert(payload.user_id, payload.key_package);

    Json({ serde_json::json!({
            "status": "ok"
        }) })
}

async fn get_keypackages(
    State(state): State<AppState>,
    Json(payload): Json<FetchKeyPackagePayload>
) -> Json<HashMap<String, Vec<u8>>> {
    println!("get_key_packages: {:?}", payload);
    let db = state.key_packages.read().await;
    let result = payload.user_ids
        .iter()
        .filter_map(|id| db.get(id).map(|kp| (id.clone(), kp.clone())))
        .collect();
    Json(result)
}

async fn log_request(request: Request<Body>, next: axum::middleware::Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let response = next.run(request).await;

    info!("{} {} -> {}", method, uri, response.status());
    response
}

async fn preflight() -> impl IntoResponse {
    Json({ serde_json::json!({
            "status": "ok"
        }) })
}

async fn sse_handler(
    State(state): State<AppState>,
    headers: HeaderMap
) -> Sse<impl futures::Stream<Item = Result<Event, axum::Error>>> {
    let user_id = headers
        .get("user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let (tx, mut rx) = mpsc::channel(100);

    {
        let mut users = state.users.write().await;
        users.insert(user_id.clone(), tx.clone());
    }

    let _ = tx.send(ChatMessage {
        from: "server".to_string(),
        to: vec!["room_id".to_string()],
        mess_type: "ping".to_string(),
        message: b"welcome to server".to_vec(),
    }).await;

    let stream = async_stream::stream! {
        while let Some(msg) = rx.recv().await {
            match Event::default().json_data(msg) {
                Ok(event) => {
                    yield Ok(event);
                }
                Err(e) => tracing::error!("Failed to serialize event: {:?}", e),
            }
        }
    };

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ChatMessage>
) -> Json<ChatMessage> {
    let room_id = headers
        .get("room-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if let Some(users_in_room) = state.rooms.read().await.get(&room_id) {
        let users = state.users.read().await;

        if payload.mess_type == "welcomeMessage" {
            for user_id in payload.to.iter() {
                if let Some(sender) = users.get(user_id) {
                    let _ = sender.send(payload.clone()).await;
                }
            }
        } else {
            for user_id in users_in_room {
                if let Some(sender) = users.get(user_id) {
                    let _ = sender.send(payload.clone()).await;
                }
            }
        }
    }
    Json(payload)
}

async fn add_user_to_room(State(state): State<AppState>, headers: HeaderMap) -> Json<&'static str> {
    let user_id = headers
        .get("user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let room_id = headers
        .get("room-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    {
        let mut rooms = state.rooms.write().await;
        rooms.entry(room_id.clone()).or_insert_with(HashSet::new).insert(user_id.clone());
    }

    Json("User added to room")
}

async fn list_rooms(State(state): State<AppState>) -> Json<Vec<String>> {
    let rooms = state.rooms.read().await;
    Json(rooms.keys().cloned().collect())
}

async fn list_room_members(State(state): State<AppState>, headers: HeaderMap) -> Json<Vec<String>> {
    let room_id = headers
        .get("room-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    println!("{room_id}");
    let rooms = state.rooms.read().await;
    if let Some(room) = rooms.get(&room_id) {
        Json(room.iter().cloned().collect())
    } else {
        Json(vec![])
    }
}

async fn list_users(State(state): State<AppState>) -> Json<Vec<String>> {
    let users = state.users.read().await;
    Json(users.keys().cloned().collect())
}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder().with_max_level(Level::INFO).finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");
    let state = AppState::default();

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any);
    let app = Router::new()
        .route(
            "/health",
            post(|| async { "OK" })
        )
        .route("/mls/upload_keypackage", post(upload_keypackage))
        .route("/mls/upload_keypackage", axum::routing::options(preflight))
        .route("/mls/get_keypackages", post(get_keypackages))
        .route("/mls/get_keypackages", axum::routing::options(preflight))
        .route("/subscribe", get(sse_handler))
        .route("/send", post(send_message))
        .route("/rooms", get(list_rooms))
        .route("/rooms/members", get(list_room_members))
        .route("/add_user_to_room", post(add_user_to_room))
        .route("/users", get(list_users))
        .layer(cors)
        .layer(axum::middleware::from_fn(log_request))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:5151").await.unwrap();
    info!("Listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
