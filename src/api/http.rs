use crate::api::handlers::{execute_query, get_device, health_check, list_devices, AppState};
use crate::api::middleware::{jwt_auth, security_headers, AuthMiddleware};
use crate::config::ApiConfig;
use crate::query::executor::QueryExecutor;
use crate::query::parser::QueryParser;
use crate::security::jwt::JwtService;
use crate::storage::StorageEngine;
use anyhow::Result;
use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

/// HTTP/HTTPS API server
pub struct HttpServer {
    app_state: AppState,
    auth_middleware: AuthMiddleware,
    bind_addr: SocketAddr,
    enable_tls: bool,
    tls_cert_path: Option<String>,
    tls_key_path: Option<String>,
}

impl HttpServer {
    pub fn new(
        storage: Arc<StorageEngine>,
        jwt_service: Arc<JwtService>,
        config: ApiConfig,
    ) -> Self {
        let query_executor = Arc::new(QueryExecutor::new(storage.clone()));
        let query_parser = Arc::new(QueryParser::new());

        let app_state = AppState {
            storage,
            query_executor,
            query_parser,
        };

        let auth_middleware = AuthMiddleware::new(jwt_service);

        Self {
            app_state,
            auth_middleware,
            bind_addr: config.bind_addr,
            enable_tls: config.enable_tls,
            tls_cert_path: config.tls_cert.map(|p| p.to_string_lossy().to_string()),
            tls_key_path: config.tls_key.map(|p| p.to_string_lossy().to_string()),
        }
    }

    /// Build the Axum router with all routes and middleware
    fn build_router(&self) -> Router {
        // Public routes (no authentication required)
        let public_routes = Router::new().route("/health", get(health_check));

        // Protected routes (authentication required)
        let protected_routes = Router::new()
            .route("/query", post(execute_query))
            .route("/devices", get(list_devices))
            .route("/devices/:dev_eui", get(get_device))
            .layer(middleware::from_fn_with_state(
                self.auth_middleware.clone(),
                jwt_auth,
            ));

        // Combine routes and apply global middleware
        Router::new()
            .merge(public_routes)
            .merge(protected_routes)
            .layer(middleware::from_fn(security_headers))
            .with_state(self.app_state.clone())
    }

    /// Start the HTTP/HTTPS server
    pub async fn serve(self) -> Result<()> {
        let app = self.build_router();

        if self.enable_tls {
            info!(
                "Starting HTTPS server on {} with TLS",
                self.bind_addr
            );

            let cert_path = self.tls_cert_path.as_ref().ok_or_else(|| {
                anyhow::anyhow!("TLS enabled but cert path not configured")
            })?;
            let key_path = self.tls_key_path.as_ref().ok_or_else(|| {
                anyhow::anyhow!("TLS enabled but key path not configured")
            })?;

            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
                cert_path,
                key_path,
            )
            .await?;

            axum_server::bind_rustls(self.bind_addr, config)
                .serve(app.into_make_service())
                .await?;
        } else {
            info!(
                "Starting HTTP server on {} (TLS disabled - use reverse proxy for HTTPS)",
                self.bind_addr
            );

            let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
            axum::serve(listener, app).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageConfig;
    use crate::security::jwt::Claims;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use hyper::http;
    use tempfile::TempDir;
    use tower::ServiceExt;

    async fn create_test_server() -> HttpServer {
        let temp_dir = TempDir::new().unwrap();
        let storage_config = StorageConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync_interval_ms: 1000,
            memtable_size_mb: 1,
            compaction_threshold: 3,
            enable_encryption: false,
            encryption_key: None,
        };

        let storage = Arc::new(StorageEngine::new(storage_config).await.unwrap());
        let jwt_service = Arc::new(
            JwtService::new("this-is-a-very-secure-secret-key-for-testing").unwrap(),
        );

        let api_config = ApiConfig {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),
            enable_tls: false,
            tls_cert: None,
            tls_key: None,
            jwt_secret: "this-is-a-very-secure-secret-key-for-testing".to_string(),
            rate_limit_per_minute: 100,
        };

        HttpServer::new(storage, jwt_service, api_config)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let server = create_test_server().await;
        let app = server.build_router();

        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_protected_endpoint_without_auth() {
        let server = create_test_server().await;
        let app = server.build_router();

        let request = Request::builder()
            .uri("/devices")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_protected_endpoint_with_auth() {
        let server = create_test_server().await;
        let app = server.build_router();

        // Generate valid JWT token
        let jwt_service = JwtService::new("this-is-a-very-secure-secret-key-for-testing").unwrap();
        let claims = Claims::new("test-user".to_string());
        let token = jwt_service.generate_token(claims).unwrap();

        let request = Request::builder()
            .uri("/devices")
            .header(http::header::AUTHORIZATION, format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_security_headers_present() {
        let server = create_test_server().await;
        let app = server.build_router();

        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Verify security headers are present
        let headers = response.headers();
        assert!(headers.contains_key(http::header::STRICT_TRANSPORT_SECURITY));
        assert!(headers.contains_key(http::header::CONTENT_SECURITY_POLICY));
        assert!(headers.contains_key(http::header::X_FRAME_OPTIONS));
    }
}
