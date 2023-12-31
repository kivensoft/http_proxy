//! 网关应用提供的服务接口

use crate::{proxy::{self, ServiceGroup}, AppGlobal};

use compact_str::{format_compact, CompactString};
use httpserver::{HttpContext, Resp, HttpResult};
use localtime::LocalTime;
use querystring::querify;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct PingRequest {
    reply: Option<CompactString>,
}

#[derive(Deserialize)]
struct RegRequest {
    endpoint: CompactString,
    path: Option<CompactString>,
    paths: Option<Vec<CompactString>>,
}

/// 服务测试，测试服务是否存活
pub async fn ping(ctx: HttpContext) -> HttpResult {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Res {
        reply: CompactString,
        now: LocalTime,
        server: CompactString,
    }

    let reply = get_reply_param(ctx).await;

    Resp::ok(&Res {
        reply,
        now: LocalTime::now(),
        server: format_compact!("{}/{}", crate::APP_NAME, crate::APP_VER),
    })
}

/// 服务状态
pub async fn status(_ctx: HttpContext) -> HttpResult {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Res {
        startup    : LocalTime,         // 应用启动时间
        services   : Vec<ServiceGroup>, // 有效服务列表
    }

    let app_global = AppGlobal::get();

    Resp::ok(&Res {
        startup: LocalTime::from_unix_timestamp(app_global.startup_time as i64 ),
        services: proxy::service_status(),
    })
}

/// 注册服务查询
pub async fn query(ctx: HttpContext) -> HttpResult {
    #[derive(Deserialize)]
    struct Req {
        path: Option<CompactString>,
        paths: Option<Vec<CompactString>>,
    }

    #[derive(Serialize)]
    struct Item {
        path: CompactString,
        services: Vec<CompactString>,
    }

    #[derive(Serialize)]
    struct Res {
        #[serde(skip_serializing_if = "Option::is_none")]
        services: Option<Vec<CompactString>>,
        list: Option<Vec<Item>>,
    }

    let url_path = CompactString::new(ctx.req.uri().path());
    let mut param = ctx.into_opt_json::<Req>().await?
            .unwrap_or(Req { path: None, paths: None });

    if param.path.is_none() && !url_path.ends_with("/query") {
        if let Some(pos) = url_path.rfind('/') {
            let val = urlencoding::decode(&url_path[pos+1..])?;
            param.path = Some(CompactString::new(val));
        }
    }

    if param.path.is_none() && param.paths.is_none() {
        return Resp::fail("param path and paths not find");
    }

    if let Some(path) = &param.path {
        log::debug!("查找: {path}");
        let services = proxy::service_query(path);
        return Resp::ok(&Res { services, list: None, })
    }

    if let Some(paths) = param.paths {
        let mut list = Vec::with_capacity(paths.len());
        for path in paths {
            if let Some(services) = proxy::service_query(&path) {
                list.push(Item {
                    path,
                    services,
                });
            }
        }
        return Resp::ok(&Res { services: None, list: Some(list) })
    }

    Resp::ok_with_empty()
}

/// 注册服务(同时也作为心跳服务使用)
pub async fn reg(ctx: HttpContext) -> HttpResult {
    type Req = RegRequest;

    #[derive(Serialize)]
    struct Res {
        endpoint: CompactString,
    }

    let param = ctx.into_json::<Req>().await?;

    if param.path.is_none() && param.paths.is_none() {
        return Resp::fail("param path and paths not find");
    }

    if let Some(path) = &param.path {
        if proxy::register_service(path, &param.endpoint) {
            log::info!("service[{}: {}] registration successful", path, param.endpoint);
        }
    }

    if let Some(paths) = &param.paths {
        for path in paths {
            if proxy::register_service(path, &param.endpoint) {
                log::info!("service[{}: {}] registration successful", path, param.endpoint);
            }
        }
    }

    Resp::ok_with_empty()
}

/// 取消服务注册
pub async fn unreg(ctx: HttpContext) -> HttpResult {
    type Req = RegRequest;

    let param = ctx.into_json::<Req>().await?;

    if param.path.is_none() && param.paths.is_none() {
        return Resp::fail("param path and paths not find");
    }

    let endpoint = &param.endpoint;

    if let Some(path) = &param.path {
        proxy::unregister_service(path, endpoint);
        log::info!("unregister service[{}: {}]", path, endpoint);
    }

    if let Some(paths) = &param.paths {
        for path in paths {
            proxy::unregister_service(path, endpoint);
            log::info!("unregister server[{}: {}]", path, endpoint);
        }
    }

    Resp::ok_with_empty()
}

/// 获取请求中reply参数, 获取优先级: post_data > query_string > url_path > default
async fn get_reply_param(ctx: HttpContext) -> CompactString {
    let path = CompactString::new(ctx.req.uri().path());
    let querystring = CompactString::new(ctx.req.uri().query().unwrap_or(""));

    if let Ok(Some(param)) = ctx.into_opt_json::<PingRequest>().await {
        if let Some(reply) = param.reply {
            if !reply.is_empty() {
                return reply;
            }
        }
    }

    if !querystring.is_empty() {
        let param = querify(&querystring);
        for item in param {
            if item.0 == "reply" {
                if let Ok(val) = urlencoding::decode(item.1) {
                    if !val.is_empty() {
                        return CompactString::new(val);
                    }
                }
            }
        }
    }

    if !path.ends_with("/ping") {
        if let Some(pos) = path.rfind('/') {
            if let Ok(val) = urlencoding::decode(&path[pos+1..]) {
                if !val.is_empty() {
                    return CompactString::new(val);
                }
            }
        }
    }

    CompactString::new("pong")
}
