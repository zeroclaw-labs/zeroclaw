use anyhow::Result;
use chrono::{DateTime, Local};
use std::fs;
use std::path::Path;
use tracing::info;
use tracing_subscriber::{
    EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

/// 初始化日志系统
pub fn init_logging(config: &cortex_mem_config::LoggingConfig) -> Result<()> {
    if !config.enabled {
        // 如果日志未启用，不设置任何tracing层
        tracing_subscriber::registry().try_init().ok(); // 避免重复初始化错误
        return Ok(());
    }

    // 创建日志目录（如果不存在）
    fs::create_dir_all(&config.log_directory)?;

    // 生成带时间戳的日志文件名
    let local_time: DateTime<Local> = Local::now();
    let log_file_name = format!("cortex-memo-{}.log", local_time.format("%Y-%m-%d-%H-%M-%S"));
    let log_file_path = Path::new(&config.log_directory).join(log_file_name);

    // 创建文件写入器
    let file_writer = std::fs::File::create(&log_file_path)?;

    // 根据配置的日志级别设置过滤器
    let level_filter = match config.level.to_lowercase().as_str() {
        "error" => "error",
        "warn" => "warn",
        "info" => "info",
        "debug" => "debug",
        "trace" => "trace",
        _ => "info", // 默认为info级别
    };

    // 只配置文件输出，不配置控制台输出
    let file_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level_filter));
    let file_layer = fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file_writer))
        .with_filter(file_filter);

    // 初始化tracing订阅者，只添加文件层，不添加控制台层
    tracing_subscriber::registry().with(file_layer).try_init()?;

    info!("Logging initialized. Log file: {}", log_file_path.display());
    Ok(())
}
