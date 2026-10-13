//! The operator recovery command must clear counters and exit without serving.
use sqlx::{Connection, PgConnection};
use std::process::Command;

#[tokio::test]
async fn reset_login_attempts_clears_the_table_and_exits_without_listening() {
    let base = std::env::var("TREER_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://treer:treer@127.0.0.1:55432/treer_test".to_string());
    let (prefix, _) = base
        .rsplit_once('/')
        .expect("database URL has a database name");
    let name = format!("reset_{}", uuid::Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&base)
        .await
        .expect("connect to test PostgreSQL");
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut admin)
        .await
        .unwrap();
    let url = format!("{prefix}/{name}");
    let mut db = PgConnection::connect(&url).await.unwrap();
    sqlx::query("CREATE TABLE login_attempts (bucket TEXT PRIMARY KEY, attempts INTEGER NOT NULL, window_started_at TEXT NOT NULL)")
        .execute(&mut db).await.unwrap();
    sqlx::query("INSERT INTO login_attempts VALUES ('admin', 99, 'x'), ('user:a', 3, 'x')")
        .execute(&mut db)
        .await
        .unwrap();
    // The command returns before authentication, listener and event-bus setup;
    // a serving Proxy would not exit on its own.
    let output = Command::new(env!("CARGO_BIN_EXE_treer-proxy"))
        .args(["--reset-login-attempts", "--database-url", &url])
        .env_remove("TREER_DISABLE_AUTH")
        .output()
        .expect("run treer-proxy");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("cleared 2 sign-in attempt counters"));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM login_attempts")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(left, 0);
    db.close().await.unwrap();
    sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&mut admin)
        .await
        .unwrap();
}
