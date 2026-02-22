use crate::auth::jwt::JwtService;
use crate::auth::rate_limit::RateLimiter;
use crate::config::PlatformConfig;
use crate::db::pool::DbPool;
use crate::docker::DockerManager;
use crate::email::EmailService;
use crate::proxy::ProxyManager;
use crate::tenant::provisioner::Provisioner;
use crate::vault::VaultService;
use std::sync::Arc;

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub config: PlatformConfig,
    pub db: DbPool,
    pub vault: VaultService,
    pub jwt: JwtService,
    pub otp_limiter: RateLimiter,
    pub docker: DockerManager,
    pub provisioner: Provisioner,
    pub proxy: Option<ProxyManager>,
    pub email: Option<EmailService>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: PlatformConfig,
        db: DbPool,
        vault: VaultService,
        jwt: JwtService,
        otp_limiter: RateLimiter,
        docker: DockerManager,
        provisioner: Provisioner,
        proxy: Option<ProxyManager>,
        email: Option<EmailService>,
    ) -> SharedState {
        Arc::new(Self {
            config,
            db,
            vault,
            jwt,
            otp_limiter,
            docker,
            provisioner,
            proxy,
            email,
        })
    }
}
