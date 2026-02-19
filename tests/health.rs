mod common;
use common::*;

#[tokio::test]
async fn health_check() {
    run_test(
        "health_check",
        |_| {},
        async |port| {
            let request_url = server_url(port).join("health").unwrap();
            let health_response = reqwest::get(request_url).await.unwrap();
            assert_eq!(health_response.status(), reqwest::StatusCode::OK);
            let health_response_text = health_response.text().await.unwrap();
            assert_eq!(health_response_text, "Ok");
        },
    )
    .await;
}
