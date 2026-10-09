use delonix_push::{api, config::Config, dispatch, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&url)
        .await
        .expect("base");
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("migrações");
    let st = AppState::new(db, Config::from_env());
    tokio::spawn(dispatch::run_worker(st.clone()));
    let bind = std::env::var("PUSH_BIND").unwrap_or_else(|_| "0.0.0.0:8480".into());
    let l = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    tracing::info!("delonix-push em {bind}");
    axum::serve(l, api::router(st)).await.unwrap();
}
