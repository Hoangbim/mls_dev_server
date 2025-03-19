use axum::{
    body::Body,
    extract::{ Query, State },
    http::{ Method, Request, StatusCode },
    response::{ sse::Event, IntoResponse, Response, Sse },
    routing::{ get, post },
    Json,
    Router,
};
use serde::{ Deserialize, Serialize };
use tracing::{ info, Level };
use std::{ collections::{ HashMap, HashSet }, sync::Arc, time::{ SystemTime, UNIX_EPOCH } };
use tower_http::{ cors::{ CorsLayer, Any }, trace::TraceLayer };
use tracing_subscriber::FmtSubscriber;
use tokio::sync::{ mpsc, RwLock };

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
    message_id: String,
    room_id: Option<String>,
    from: String, // sender
    to: Vec<String>, //group id
    mess_type: String, // welcome, encrypt_message, ping
    message: Vec<u8>, // mls encrypt message or string bytes
    // created_at: u64,
    created_at: String,
}
#[derive(Clone, Default)]
struct AppState {
    key_packages: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    users: Arc<RwLock<HashMap<String, mpsc::Sender<ChatMessage>>>>,
    rooms: Arc<RwLock<HashMap<String, HashSet<String>>>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ConnectSseQuery {
    user_id: Option<String>,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct GetRoomMemberQuery {
    room_id: String,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct AddUserToRoom {
    user_ids: Vec<String>,
    room_id: String,
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

async fn sse_handler(
    State(state): State<AppState>,
    Query(query): Query<ConnectSseQuery>
) -> Result<
    Sse<impl futures::Stream<Item = Result<Event, axum::Error>>>,
    (StatusCode, &'static str)
> {
    let user_id = match &query.user_id {
        Some(id) if !id.is_empty() => id.clone(),
        _ => {
            return Err((StatusCode::BAD_REQUEST, "user_id is required"));
        }
    };

    println!("User ID connected to SSE: {user_id}");

    let (tx, mut rx) = mpsc::channel(100);

    {
        let mut users = state.users.write().await;
        users.insert(user_id.clone(), tx.clone());
    }

    let _ = tx.send(ChatMessage {
        message_id: uuid::Uuid::new_v4().to_string(),
        room_id: None,
        from: "server".to_string(),
        to: vec!["room_id".to_string()],
        mess_type: "ping".to_string(),
        message: b"Welcome to server".to_vec(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis()
            .to_string(),
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

    Ok(Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

async fn send_message(
    State(state): State<AppState>,
    Json(payload): Json<ChatMessage>
) -> impl IntoResponse {
    let users = state.users.read().await;

    if payload.mess_type == "WelcomeMessage" {
        for user_id in &payload.to {
            if let Some(sender) = users.get(user_id) {
                let _ = sender.send(payload.clone()).await;
            }
        }
    } else {
        println!("payload: {:#?}", payload);
        let room_id = match &payload.room_id {
            Some(room_id) => room_id,
            None => {
                return Err((StatusCode::BAD_REQUEST, "room_id is required "));
            }
        };

        let rooms = state.rooms.read().await;
        println!("room in db:{:?}", rooms);
        if let Some(users_in_room) = rooms.get(room_id) {
            for user_id in users_in_room {
                if payload.from.as_bytes() != user_id.as_bytes() {
                    if let Some(sender) = users.get(user_id) {
                        let _ = sender.send(payload.clone()).await;
                    }
                }
            }
        }
    }

    Ok(Json(payload))
}

async fn add_user_to_room(
    State(state): State<AppState>,
    Json(payload): Json<AddUserToRoom>
) -> Json<&'static str> {
    let user_ids = payload.user_ids;

    let room_id = payload.room_id;

    {
        let mut rooms = state.rooms.write().await;
        let room_members = rooms.entry(room_id.clone()).or_insert_with(HashSet::new);
        for user_id in user_ids {
            println!("Users: {user_id} was added to room: {room_id}");
            room_members.insert(user_id.clone());
        }
    }
    Json("Users was added to room:")
}

async fn list_rooms(State(state): State<AppState>) -> Json<Vec<String>> {
    let rooms = state.rooms.read().await;
    Json(rooms.keys().cloned().collect())
}

async fn list_room_members(
    State(state): State<AppState>,
    Query(query): Query<GetRoomMemberQuery>
) -> Json<Vec<String>> {
    let room_id = query.room_id;
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
        // .route("/mls/upload_keypackage", axum::routing::options(preflight))
        .route("/mls/get_keypackages", post(get_keypackages))
        // .route("/mls/get_keypackages", axum::routing::options(preflight))
        // .route("/subscribe/{user_id}", get(sse_handler))
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
