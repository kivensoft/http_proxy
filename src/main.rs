mod apis;
mod proxy;

use std::{fmt::Write, time::{SystemTime, Duration}};

use anyhow::Context;
use compact_str::format_compact;
use httpserver::HttpServer;
use smallstr::SmallString;

const BANNER: &str = r#"
    __  ____  __        ____
   / / / / /_/ /_____  / __ \_________  _  ____  __
  / /_/ / __/ __/ __ \/ /_/ / ___/ __ \| |/_/ / / /
 / __  / /_/ /_/ /_/ / ____/ /  / /_/ />  </ /_/ /
/_/ /_/\__/\__/ .___/_/   /_/   \____/_/|_|\__, /
             /_/  Kivensoft Ver: %        /____/
"#;

const APP_NAME: &str = "httpproxy";
/// app版本号, 来自编译时由build.rs从cargo.toml中读取的版本号(读取内容写入.version文件)
const APP_VER: &str = include_str!(concat!(env!("OUT_DIR"), "/.version"));

appconfig::appglobal_define!(app_global, AppGlobal,
    connect_timeout: u32,
    startup_time: u64,
);

appconfig::appconfig_define!(app_conf, AppConf,
    log_level   : String => ["L",  "log-level",    "LogLevel",          "日志级别(trace/debug/info/warn/error/off)"],
    log_file    : String => ["F",  "log-file",     "LogFile",           "日志的相对路径或绝对路径文件名"],
    log_max     : String => ["M",  "log-max",      "LogFileMaxSize",    "日志文件的最大长度 (单位: k|m|g)"],
    log_async   : bool   => ["",   "log-async",    "LogAsync",          "启用异步日志"],
    no_console  : bool   => ["",   "no-console",   "NoConsole",         "禁止将日志输出到控制台"],
    threads     : String => ["t",  "threads",      "Threads",           "设置应用的线程数"],
    listen      : String => ["l",  "listen",       "Listen",            "服务监听端点 (ip地址:端口号)"],
    conn_timeout: String => ["",   "conn-timeout", "ConnectTimeout",    "连接超时时间(单位: 秒)"],
    gw_path     : String => ["p",  "gw-path",      "GwPath",            "本地服务路径"],
);

impl Default for AppConf {
    fn default() -> Self {
        Self {
            log_level:    String::from("info"),
            log_file:     String::with_capacity(0),
            log_max:      String::from("10m"),
            log_async:    false,
            no_console:   false,
            threads:      String::from("1"),
            listen:       String::from("127.0.0.1:3003"),
            conn_timeout: String::from("3"),
            gw_path:      String::from("/api/gw"),
        }
    }
}

macro_rules! arg_err {
    ($text:literal) => {
        concat!("参数 ", $text, " 格式错误")
    };
}

/// 获取当前时间基于UNIX_EPOCH的秒数
fn unix_timestamp() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()
}

fn init() -> Option<(&'static mut AppConf, &'static mut AppGlobal)> {
    let mut buf = SmallString::<[u8; 512]>::new();

    write!(buf, "{APP_NAME} 版本 {APP_VER} 版权所有 Kivensoft 2023.").unwrap();
    let version = buf.as_str();

    let ac = AppConf::init();
    if !appconfig::parse_args(ac, version).expect("解析参数失败") {
        return None;
    }

    let ag = AppGlobal::init(AppGlobal {
        connect_timeout: ac.conn_timeout.parse().expect(arg_err!("conn-timeout")),
        startup_time: unix_timestamp(),
    });

    if !ac.listen.is_empty() && ac.listen.as_bytes()[0] == b':' {
        ac.listen.insert_str(0, "0.0.0.0");
    };

    let log_level = asynclog::parse_level(&ac.log_level).expect(arg_err!("log-level"));
    let log_max = asynclog::parse_size(&ac.log_max).expect(arg_err!("log-max"));

    if log_level == log::Level::Trace {
        println!("config setting: {ac:#?}\n");
    }

    asynclog::init_log(log_level, ac.log_file.clone(), log_max,
        !ac.no_console, ac.log_async).expect("初始化日志错误");
    asynclog::set_level("mio".to_owned(), log::LevelFilter::Info);
    asynclog::set_level("want".to_owned(), log::LevelFilter::Info);

    if let Some((s1, s2)) = BANNER.split_once('%') {
        let s2 = &s2[APP_VER.len() - 1..];
        buf.clear();
        write!(buf, "{s1}{APP_VER}{s2}").unwrap();
        appconfig::print_banner(&buf, true);
    }

    Some((ac, ag))
}

// #[tokio::main(worker_threads = 4)]
// #[tokio::main(flavor = "current_thread")]
fn main() {
    let (ac, _ag) = match init() {
        Some((ac, ag)) => (ac, ag),
        None => return,
    };
    log::info!("正在启动{}服务...", APP_NAME);

    let addr: std::net::SocketAddr = ac.listen.parse().unwrap();

    let mut srv = HttpServer::new("", true);
    srv.default_handler(proxy::proxy_handler);
    // srv.middleware(ProxyLog);

    proxy::init_client(Some(Duration::from_secs(
        AppGlobal::get().connect_timeout as u64,
    )));

    let gw_path = format_compact!("{}/", ac.gw_path);
    httpserver::register_apis!(srv, gw_path,
        "ping": apis::ping,
        "ping/*": apis::ping,
        "status": apis::status,
        "query": apis::query,
        "query/*": apis::query,
        "reg": apis::reg,
        "unreg": apis::unreg,
    );

    let async_fn = async move {
        // 运行http server主服务
        srv.run(addr).await.context("http服务运行失败").unwrap();
    };

    let threads = ac.threads.parse::<usize>().expect(arg_err!("threads"));

    cfg_if::cfg_if! {
        if #[cfg(not(feature = "multi_thread"))] {
            assert!(threads == 1, "{APP_NAME}当前版本不支持多线程");

            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async_fn);
        } else {
            assert!(threads >= 0 && threads <= 256, "线程数量范围: 0-256");

            let mut builder = tokio::runtime::Builder::new_multi_thread();
            if threads > 0 {
                builder.worker_threads(threads);
            }

            builder.enable_all()
                .build()
                .unwrap()
                .block_on(async_fn)
        }
    }
}
