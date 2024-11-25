use std::io::Read;
use std::{str::FromStr, collections::BTreeMap};
use http_body_util::BodyExt;
use hyper::http::{HeaderValue, HeaderName};
use tokio_util::bytes::Buf;
use crate::cqevent::do_script;
use crate::redlang::exfun::get_raw_data;
use crate::RT_PTR;
use crate::cqapi::cq_add_log_w;
use crate::{redlang::RedLang, read_code_cache};
use hyper::body::Bytes;

type GenericError = Box<dyn std::error::Error + Send + Sync>;
type BoxBody = http_body_util::combinators::BoxBody<Bytes,GenericError>;

fn get_script_info<'a>(script_json:&'a serde_json::Value) -> Result<(&'a str,&'a str,&'a str,&'a str,&'a str,&'a str), Box<dyn std::error::Error>>{
    let pkg_name_opt = script_json.get("pkg_name");
    let mut pkg_name = "";
    if let Some(val) = pkg_name_opt {
        pkg_name = val.as_str().ok_or("pkg_name不是字符串")?;
    }
    let name = script_json.get("name").ok_or("脚本中无name")?.as_str().ok_or("脚本中name不是str")?;
    let node = script_json.get("content").ok_or("script.json文件缺少content字段")?;
    let keyword = node.get("关键词").ok_or("脚本中无关键词")?.as_str().ok_or("脚本中关键词不是str")?;
    let cffs = node.get("触发方式").ok_or("脚本中无触发方式")?.as_str().ok_or("脚本中触发方式不是str")?;
    let code = node.get("code").ok_or("脚本中无code")?.as_str().ok_or("脚本中code不是str")?;
    let ppfs = node.get("匹配方式").ok_or("脚本中无匹配方式")?.as_str().ok_or("脚本中匹配方式不是str")?;
    
    return Ok((keyword,cffs,code,ppfs,name,pkg_name));
}

pub fn get_params_from_uri(uri:&hyper::Uri) -> BTreeMap<String,String> {
    let mut ret_map = BTreeMap::new();
    if uri.query().is_none() {
        return ret_map;
    }
    let query_str = uri.query().unwrap();
    let query_vec = query_str.split("&");
    for it in query_vec {
        if it == "" {
            continue;
        }
        let index_opt = it.find("=");
        if index_opt.is_some() {
            let k_rst: String = url::form_urlencoded::parse(it.get(0..index_opt.unwrap()).unwrap().as_bytes())
                .map(|(key, val)| [key, val].concat())
                .collect::<String>();
            let v_rst: String = url::form_urlencoded::parse(it.get(index_opt.unwrap() + 1..).unwrap().as_bytes())
                .map(|(key, val)| [key, val].concat())
                .collect::<String>();
            ret_map.insert(k_rst, v_rst);
        }
        else {
            let k_rst: String = url::form_urlencoded::parse(it.as_bytes())
                .map(|(key, val)| [key, val].concat())
                .collect::<String>();
            ret_map.insert(k_rst,"".to_owned());
        }
    }
    ret_map
}

pub fn do_http_event(req:hyper::Request<hyper::body::Incoming>,can_write:bool,can_read:bool) -> Result<hyper::Response<BoxBody>, Box<dyn std::error::Error>> {
     // 获取pkg_name和pkg_key
    let url_path = req.uri().path();
    let true_url = url_path.get(5..).unwrap();
    let splited_url = true_url.split('/').into_iter().collect::<Vec<&str>>();
    let pkg_name = splited_url.get(1).ok_or("无法得到包名")?;
    let pkg_key = true_url.get(pkg_name.len() + 1..).unwrap();
    let pkg_name_t: String = url::form_urlencoded::parse(pkg_name.as_bytes())
        .map(|(key, val)| [key, val].concat())
        .collect::<String>();
    let msg: String = url::form_urlencoded::parse(pkg_key.as_bytes())
        .map(|(key, val)| [key, val].concat())
        .collect::<String>();
    let script_json = read_code_cache()?;
    let method = req.method().to_string();
    let mut req_headers = BTreeMap::new();
    for it in req.headers() {
        req_headers.insert(it.0.as_str().to_owned(), it.1.to_str()?.to_owned());
    }
    let uri = req.uri();
    let req_params = get_params_from_uri(uri);
    let (body_tx1,mut body_rx1) =  tokio::sync::mpsc::channel(1);
    let (body_tx2, body_rx2) =  tokio::sync::mpsc::channel(1);
    RT_PTR.spawn(async move {
        let ret = body_rx1.recv().await;
        if ret.is_some() {

            let bt_rst = req.collect().await;
            if let Ok(bt) = bt_rst {
                let mut body_reader = bt.aggregate().reader();
                let mut body = Vec::new();
                let bt2_rst = body_reader.read_to_end(&mut body);
                if bt2_rst.is_err() {
                    cq_add_log_w(&format!("获取访问体失败:{bt2_rst:?}")).unwrap();
                    let _foo = body_tx2.send(vec![]).await;
                }else {
                    let _foo = body_tx2.send(body).await;
                }
                
            }else {
                cq_add_log_w(&format!("获取访问体失败:{bt_rst:?}")).unwrap();
                let _foo = body_tx2.send(vec![]).await;
            }
        }
    });
    let web_access;
    if can_write {
        web_access = "可写";
    } else if can_read {
        web_access = "只读";
    } else {
        web_access = "";
    }
    for i in 0..script_json.as_array().ok_or("script.json文件不是数组格式")?.len(){
        let (keyword,cffs,code,ppfs,name,pkg_name) = get_script_info(&script_json[i])?;
        let mut rl = RedLang::new();
        if cffs == "网络触发" && pkg_name == pkg_name_t && crate::cqevent::is_key_match(&mut rl,&ppfs,keyword,&msg)? {
            rl.set_coremap("网络-访问方法", &method)?;
            rl.set_coremap("网络-访问参数", &rl.build_obj(req_params))?;
            rl.set_coremap("网络-访问头", &rl.build_obj(req_headers))?;
            rl.set_coremap("网络-权限",web_access)?;
            rl.req_tx = Some(body_tx1);
            rl.req_rx = Some(body_rx2);
            rl.pkg_name = pkg_name.to_owned();
            rl.script_name = name.to_owned();
            rl.can_wrong = true;
            let mut rl_ret = do_script(&mut rl, code,"normal",true)?;
            if rl_ret.contains("B96ad849c-8e7e-7886-7742-e4e896cc5b86") {
                rl_ret = get_raw_data(&mut rl, rl_ret)?;
            }
            let mut http_header = BTreeMap::new();
            let mut res:hyper::Response<BoxBody>;
            if rl.get_type(&rl_ret)? == "字节集" {
                http_header.insert("Content-Type", "application/octet-stream");
                res = hyper::Response::new(crate::httpserver::full(RedLang::parse_bin(&mut rl.bin_pool,&rl_ret)?));
            } else {
                http_header.insert("Content-Type", "text/html; charset=utf-8");
                res = hyper::Response::new(crate::httpserver::full(rl_ret));
            }
            let http_header_str = rl.get_coremap("网络-返回头")?;
            if http_header_str != "" {
                let http_header_t = RedLang::parse_obj(&http_header_str)?;
                for (k,v) in &http_header_t {
                    http_header.insert(k, v);
                }
                for (key,val) in &http_header {
                    res.headers_mut().append(HeaderName::from_str(key)?, HeaderValue::from_str(val)?);
                }
            }
            return Ok(res);
        }
    }
    let mut res:hyper::Response<BoxBody> = hyper::Response::new(crate::httpserver::full("api not found"));
    res.headers_mut().insert("Content-Type", HeaderValue::from_static("text/html; charset=utf-8"));
    Ok(res)
}