use delonix_push::{api, config::Config, dispatch, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(
            std::env::var("PUSH_DB_MAX_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
        )
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                // Opcional (PUSH_ASYNC_COMMIT=1): o COMMIT não espera pelo disco. Duplica o débito de escrita (medido) à custa
                // de, num crash do Postgres, poder perder as últimas dezenas de ms de mensagens aceites. Desligado por omissão.
                if std::env::var("PUSH_ASYNC_COMMIT").is_ok_and(|v| v == "1") {
                    sqlx::query("SET synchronous_commit = off")
                        .execute(&mut *conn)
                        .await?;
                }
                Ok(())
            })
        })
        .connect(&url)
        .await
        .expect("base");
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("migrações");
    let mut st = AppState::new(db, Config::from_env());
    if let Ok(url) = std::env::var("PUSH_REDIS_URL") {
        st.use_redis(&url).await.expect("Redis");
    }
    // PUSH_WORKER=0 desliga o reenvio/expiração nesta instância (só para experiências: sem ele nada reenvia).
    if std::env::var("PUSH_WORKER").map_or(true, |v| v != "0") {
        tokio::spawn(dispatch::run_worker(st.clone()));
    }
    tokio::spawn(dispatch::run_listener(st.clone()));
    let bind = std::env::var("PUSH_BIND").unwrap_or_else(|_| "0.0.0.0:8480".into());
    let l = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    tracing::info!("delonix-push em {bind}");
    axum::serve(l, api::router(st)).await.unwrap();
}
