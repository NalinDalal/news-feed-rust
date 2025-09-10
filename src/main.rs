use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use uuid::Uuid;
use warp::{Filter, Reply};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    id: String,
    username: String,
    profile_picture: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Post {
    id: String,
    user_id: String,
    content: String,
    image_url: Option<String>,
    video_url: Option<String>,
    timestamp: u64,
    like_count: u32,
    reply_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NewsFeedItem {
    post_id: String,
    timestamp: u64,
}

#[derive(Debug, Clone)]
struct FanoutMessage {
    post_id: String,
    user_id: String,
    friend_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct HydratedPost {
    #[serde(flatten)]
    post: Post,
    author: Option<Author>,
    liked: bool,
}

#[derive(Debug, Serialize)]
struct Author {
    username: String,
    profile_picture: String,
}

#[derive(Debug, Serialize)]
struct Counters {
    likes: u32,
    replies: u32,
}

// Cache Layer using DashMap for thread-safe concurrent access
#[derive(Debug)]
struct CacheLayer {
    news_feeds: DashMap<String, VecDeque<NewsFeedItem>>,
    posts: DashMap<String, Post>,
    users: DashMap<String, User>,
    hot_cache: DashMap<String, Post>,
    social_graph: DashMap<String, HashSet<String>>,
    actions: DashMap<String, HashMap<String, bool>>, // userId -> postId -> liked
    counters: DashMap<String, Counters>,
}

impl CacheLayer {
    fn new() -> Self {
        Self {
            news_feeds: DashMap::new(),
            posts: DashMap::new(),
            users: DashMap::new(),
            hot_cache: DashMap::new(),
            social_graph: DashMap::new(),
            actions: DashMap::new(),
            counters: DashMap::new(),
        }
    }

    // News Feed Cache
    fn get_news_feed(&self, user_id: &str) -> Vec<NewsFeedItem> {
        self.news_feeds
            .get(user_id)
            .map(|feed| feed.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn add_to_news_feed(&self, user_id: &str, item: NewsFeedItem) {
        let mut feed = self.news_feeds.entry(user_id.to_string()).or_insert_with(VecDeque::new);
        feed.push_front(item);
        
        // Keep only latest 1000 items
        if feed.len() > 1000 {
            feed.truncate(1000);
        }
    }

    // Post Cache
    fn get_post(&self, post_id: &str) -> Option<Post> {
        self.hot_cache.get(post_id)
            .or_else(|| self.posts.get(post_id))
            .map(|entry| entry.clone())
    }

    fn set_post(&self, post: Post) {
        // Popular posts go to hot cache
        if post.like_count > 100 {
            self.hot_cache.insert(post.id.clone(), post.clone());
        }
        self.posts.insert(post.id.clone(), post);
    }

    // User Cache
    fn get_user(&self, user_id: &str) -> Option<User> {
        self.users.get(user_id).map(|entry| entry.clone())
    }

    fn set_user(&self, user: User) {
        self.users.insert(user.id.clone(), user);
    }

    // Social Graph
    fn get_followers(&self, user_id: &str) -> Vec<String> {
        let key = format!("followers_{}", user_id);
        self.social_graph
            .get(&key)
            .map(|followers| followers.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn get_following(&self, user_id: &str) -> Vec<String> {
        let key = format!("following_{}", user_id);
        self.social_graph
            .get(&key)
            .map(|following| following.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn add_follower(&self, user_id: &str, follower_id: &str) {
        let followers_key = format!("followers_{}", user_id);
        let following_key = format!("following_{}", follower_id);

        self.social_graph
            .entry(followers_key)
            .or_insert_with(HashSet::new)
            .insert(follower_id.to_string());

        self.social_graph
            .entry(following_key)
            .or_insert_with(HashSet::new)
            .insert(user_id.to_string());
    }

    // Actions
    fn like_post(&self, user_id: &str, post_id: &str) {
        // Record user action
        self.actions
            .entry(user_id.to_string())
            .or_insert_with(HashMap::new)
            .insert(post_id.to_string(), true);

        // Update counters
        let mut counters = self.counters
            .entry(post_id.to_string())
            .or_insert_with(|| Counters { likes: 0, replies: 0 });
        counters.likes += 1;

        // Update post cache if exists
        if let Some(mut post_entry) = self.posts.get_mut(post_id) {
            post_entry.like_count = counters.likes;
        }
        if let Some(mut hot_post_entry) = self.hot_cache.get_mut(post_id) {
            hot_post_entry.like_count = counters.likes;
        }
    }

    fn has_liked(&self, user_id: &str, post_id: &str) -> bool {
        // Avoid returning a reference to a temporary by cloning the HashMap
        self.actions
            .get(user_id)
            .map(|actions| actions.get(post_id).copied().unwrap_or(false))
            .unwrap_or(false)
    }

    fn get_counters(&self, post_id: &str) -> Counters {
        self.counters
            .get(post_id)
            .map(|c| Counters { likes: c.likes, replies: c.replies })
            .unwrap_or(Counters { likes: 0, replies: 0 })
    }
}

// Message Queue and Worker
struct FanoutWorker {
    id: usize,
    cache: Arc<CacheLayer>,
}

impl FanoutWorker {
    fn new(id: usize, cache: Arc<CacheLayer>) -> Self {
        Self { id, cache }
    }

    async fn process(&self, message: FanoutMessage) {
        println!("Worker {} processing fanout for post {}", self.id, message.post_id);

        let news_feed_item = NewsFeedItem {
            post_id: message.post_id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        };

        // Add to each friend's news feed
        for friend_id in &message.friend_ids {
            self.cache.add_to_news_feed(friend_id, news_feed_item.clone());
        }

        // Simulate processing time
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}

struct MessageQueue {
    sender: mpsc::UnboundedSender<FanoutMessage>,
}

impl MessageQueue {
    fn new(cache: Arc<CacheLayer>, worker_count: usize) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<FanoutMessage>();

        // Spawn one dispatcher task
        tokio::spawn({
            let cache = cache.clone();
            async move {
                let mut worker_id = 0;
                while let Some(message) = receiver.recv().await {
                    // Round-robin worker assignment
                    let worker = Arc::new(FanoutWorker::new(worker_id, cache.clone()));
                    let worker_clone = worker.clone();
                    tokio::spawn(async move {
                        worker_clone.process(message).await;
                    });
                    worker_id = (worker_id + 1) % worker_count;
                }
            }
        });

        Self { sender }
    }

    fn enqueue(&self, message: FanoutMessage) -> Result<(), &'static str> {
        self.sender
            .send(message)
            .map_err(|_| "Failed to enqueue message")
    }
}


// Services
struct PostService {
    cache: Arc<CacheLayer>,
}

impl PostService {
    fn new(cache: Arc<CacheLayer>) -> Self {
        Self { cache }
    }

    async fn create_post(
        &self,
        user_id: &str,
        content: &str,
        image_url: Option<String>,
        video_url: Option<String>,
    ) -> Post {
        let post = Post {
            id: format!("post_{}", Uuid::new_v4()),
            user_id: user_id.to_string(),
            content: content.to_string(),
            image_url,
            video_url,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            like_count: 0,
            reply_count: 0,
        };

        self.cache.set_post(post.clone());
        println!("Post created: {}", post.id);
        post
    }

    async fn get_post(&self, post_id: &str) -> Option<Post> {
        self.cache.get_post(post_id)
    }
}

struct FanoutService {
    cache: Arc<CacheLayer>,
    message_queue: Arc<MessageQueue>,
}

impl FanoutService {
    fn new(cache: Arc<CacheLayer>, message_queue: Arc<MessageQueue>) -> Self {
        Self { cache, message_queue }
    }

    async fn fanout_post(&self, post_id: &str, user_id: &str) -> Result<(), &'static str> {
        println!("Starting fanout for post {}", post_id);

        let followers = self.cache.get_followers(user_id);

        if followers.is_empty() {
            println!("No followers found for user {}", user_id);
            return Ok(());
        }

        let message = FanoutMessage {
            post_id: post_id.to_string(),
            user_id: user_id.to_string(),
            friend_ids: followers,
        };

        self.message_queue.enqueue(message)
    }
}

struct NewsFeedService {
    cache: Arc<CacheLayer>,
}

impl NewsFeedService {
    fn new(cache: Arc<CacheLayer>) -> Self {
        Self { cache }
    }

    async fn get_news_feed(&self, user_id: &str, limit: usize) -> Vec<HydratedPost> {
        let feed_items = self.cache.get_news_feed(user_id);
        let limited_items: Vec<_> = feed_items.into_iter().take(limit).collect();

        let mut hydrated_feed = Vec::new();

        for item in limited_items {
            if let Some(post) = self.cache.get_post(&item.post_id) {
                let author = self.cache.get_user(&post.user_id).map(|user| Author {
                    username: user.username,
                    profile_picture: user.profile_picture,
                });

                let counters = self.cache.get_counters(&post.id);
                let liked = self.cache.has_liked(user_id, &post.id);

                let mut hydrated_post = post.clone();
                hydrated_post.like_count = counters.likes;
                hydrated_post.reply_count = counters.replies;

                hydrated_feed.push(HydratedPost {
                    post: hydrated_post,
                    author,
                    liked,
                });
            }
        }

        hydrated_feed
    }
}

// HTTP Request/Response structs
#[derive(Debug, Deserialize)]
struct CreatePostRequest {
    content: String,
    image_url: Option<String>,
    video_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreatePostResponse {
    success: bool,
    post_id: String,
}

#[derive(Debug, Serialize)]
struct GetFeedResponse {
    feed: Vec<HydratedPost>,
}

#[derive(Debug, Deserialize)]
struct FollowUserRequest {
    target_user_id: String,
}

#[derive(Debug, Deserialize)]
struct LikePostRequest {
    post_id: String,
}

#[derive(Debug, Serialize)]
struct SuccessResponse {
    success: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

// Application State
#[derive(Clone)]
struct AppState {
    cache: Arc<CacheLayer>,
    post_service: Arc<PostService>,
    fanout_service: Arc<FanoutService>,
    news_feed_service: Arc<NewsFeedService>,
}

// Authentication middleware
fn extract_user_id(auth_token: Option<String>) -> Result<String, warp::Rejection> {
    match auth_token {
        Some(token) if token.starts_with("user_") => {
            Ok(token.replace("user_", ""))
        }
        _ => Err(warp::reject::custom(AuthError)),
    }
}

#[derive(Debug)]
struct AuthError;
impl warp::reject::Reject for AuthError {}

async fn handle_rejection(err: warp::Rejection) -> Result<impl Reply, std::convert::Infallible> {
    if err.find::<AuthError>().is_some() {
        Ok(warp::reply::with_status(
            warp::reply::json(&ErrorResponse {
                error: "Authentication required".to_string(),
            }),
            warp::http::StatusCode::UNAUTHORIZED,
        ))
    } else {
        Ok(warp::reply::with_status(
            warp::reply::json(&ErrorResponse {
                error: "Internal server error".to_string(),
            }),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ))
    }
}

// Route handlers
async fn create_post_handler(
    user_id: String,
    request: CreatePostRequest,
    state: AppState,
) -> Result<impl Reply, warp::Rejection> {
    let post = state
        .post_service
        .create_post(&user_id, &request.content, request.image_url, request.video_url)
        .await;

    if let Err(e) = state.fanout_service.fanout_post(&post.id, &user_id).await {
        eprintln!("Fanout failed: {}", e);
    }

    Ok(warp::reply::json(&CreatePostResponse {
        success: true,
        post_id: post.id,
    }))
}

async fn get_feed_handler(
    user_id: String,
    state: AppState,
) -> Result<impl Reply, warp::Rejection> {
    let feed = state.news_feed_service.get_news_feed(&user_id, 20).await;
    Ok(warp::reply::json(&GetFeedResponse { feed }))
}

async fn follow_user_handler(
    user_id: String,
    request: FollowUserRequest,
    state: AppState,
) -> Result<impl Reply, warp::Rejection> {
    state.cache.add_follower(&request.target_user_id, &user_id);
    Ok(warp::reply::json(&SuccessResponse { success: true }))
}

async fn like_post_handler(
    user_id: String,
    request: LikePostRequest,
    state: AppState,
) -> Result<impl Reply, warp::Rejection> {
    state.cache.like_post(&user_id, &request.post_id);
    Ok(warp::reply::json(&SuccessResponse { success: true }))
}

fn init_sample_data(cache: &CacheLayer) {
    // Create sample users
    cache.set_user(User {
        id: "user1".to_string(),
        username: "alice".to_string(),
        profile_picture: "https://example.com/alice.jpg".to_string(),
    });
    cache.set_user(User {
        id: "user2".to_string(),
        username: "bob".to_string(),
        profile_picture: "https://example.com/bob.jpg".to_string(),
    });
    cache.set_user(User {
        id: "user3".to_string(),
        username: "charlie".to_string(),
        profile_picture: "https://example.com/charlie.jpg".to_string(),
    });

    // Create some follow relationships
    cache.add_follower("user1", "user2"); // Bob follows Alice
    cache.add_follower("user1", "user3"); // Charlie follows Alice
    cache.add_follower("user2", "user3"); // Charlie follows Bob
}

#[tokio::main]
async fn main() {
    // Initialize services
    let cache = Arc::new(CacheLayer::new());
    let message_queue = Arc::new(MessageQueue::new(cache.clone(), 5));
    let post_service = Arc::new(PostService::new(cache.clone()));
    let fanout_service = Arc::new(FanoutService::new(cache.clone(), message_queue.clone()));
    let news_feed_service = Arc::new(NewsFeedService::new(cache.clone()));

    let state = AppState {
        cache: cache.clone(),
        post_service,
        fanout_service,
        news_feed_service,
    };

    // Initialize sample data
    init_sample_data(&cache);

    // Authentication filter
    let auth = warp::header::optional::<String>("authorization")
        .or(warp::query::<HashMap<String, String>>().map(|params: HashMap<String, String>| {
            params.get("auth_token").cloned()
        }))
        .unify()
        .and_then(|auth: Option<String>| async move {
            extract_user_id(auth)
        });

    // Routes
    let create_post = warp::post()
        .and(warp::path!("v1" / "me" / "feed"))
        .and(auth.clone())
        .and(warp::body::json())
        .and(warp::any().map({
            let state = state.clone();
            move || state.clone()
        }))
        .and_then(create_post_handler);

    let get_feed = warp::get()
        .and(warp::path!("v1" / "me" / "feed"))
        .and(auth.clone())
        .and(warp::any().map({
            let state = state.clone();
            move || state.clone()
        }))
        .and_then(get_feed_handler);

    let follow_user = warp::post()
        .and(warp::path!("v1" / "users" / "follow"))
        .and(auth.clone())
        .and(warp::body::json())
        .and(warp::any().map({
            let state = state.clone();
            move || state.clone()
        }))
        .and_then(follow_user_handler);

    let like_post = warp::post()
        .and(warp::path!("v1" / "posts" / "like"))
        .and(auth.clone())
        .and(warp::body::json())
        .and(warp::any().map({
            let state = state.clone();
            move || state.clone()
        }))
        .and_then(like_post_handler);

    let routes = create_post
        .or(get_feed)
        .or(follow_user)
        .or(like_post)
        .recover(handle_rejection);

    println!("News Feed server running on port 3030");
    println!("API Endpoints:");
    println!("POST /v1/me/feed?auth_token=user_1 - Create post");
    println!("GET /v1/me/feed?auth_token=user_2 - Get news feed");
    println!("POST /v1/users/follow?auth_token=user_1 - Follow user");
    println!("POST /v1/posts/like?auth_token=user_1 - Like post");
    println!();
    println!("Example usage:");
    println!("# Create post");
    println!(r#"curl -X POST "http://localhost:3030/v1/me/feed?auth_token=user_1" -H "Content-Type: application/json" -d '{{"content":"Hello from Rust!"}}')"#);
    println!();
    println!("# Get feed");
    println!(r#"curl "http://localhost:3030/v1/me/feed?auth_token=user_2""#);
    println!();
    println!("# Follow user");
    println!(r#"curl -X POST "http://localhost:3030/v1/users/follow?auth_token=user_2" -H "Content-Type: application/json" -d '{{"target_user_id":"user1"}}')"#);
    println!();
    println!("# Like post");
    println!(r#"curl -X POST "http://localhost:3030/v1/posts/like?auth_token=user_2" -H "Content-Type: application/json" -d '{{"post_id":"post_123"}}')"#);

    warp::serve(routes)
        .run(([127, 0, 0, 1], 3030))
        .await;
}
