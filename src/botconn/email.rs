use std::{net::TcpStream, sync::{atomic::AtomicBool, Arc}, thread, time::{Duration, SystemTime}};
use uuid::Uuid;

use super::BotConnectTrait;
use crate::{botconn::async_trait, cqapi::cq_add_log, mytool::read_json_str, RT_PTR};
use crate::mytool::str_msg_to_arr;
use lettre::{AsyncSmtpTransport, AsyncTransport};
use lettre::Tokio1Executor;

#[derive(Debug,Clone)]
pub struct EmailConnect {
    pub self_id:Arc<std::sync::RwLock<String>>,
    pub smtp_server:Arc<std::sync::RwLock<String>>,
    pub smtp_port:Arc<std::sync::RwLock<u16>>,
    pub password:Arc<std::sync::RwLock<String>>,
    pub url:String,
    pub is_stop:Arc<AtomicBool>
}

impl EmailConnect {
    pub fn build(url:&str) -> Self {
        EmailConnect {
            self_id:Arc::new(std::sync::RwLock::new("".to_owned())),
            smtp_server:Arc::new(std::sync::RwLock::new("".to_owned())),
            smtp_port:Arc::new(std::sync::RwLock::new(465)),
            password:Arc::new(std::sync::RwLock::new("".to_owned())),
            url:url.to_owned(),
            is_stop:Arc::new(AtomicBool::new(false)),
        }
    }

    
    fn deal_fetcharr(&self,fetch:&imap::types::ZeroCopy<Vec<imap::types::Fetch>>) -> Result<(), Box<dyn std::error::Error + Send + Sync>>{
        if self.is_stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        let self_id = self.self_id.read().unwrap().to_owned();
        let message = fetch.iter().next().ok_or("no message")?;
        let body = message.body().ok_or("no body")?;
        let message = mail_parser::MessageParser::default().parse(body).ok_or("mail_parser err")?;
        let sender = message.from().ok_or("sender can't get1")?.first().ok_or("sender can't get2")?;
        let user_id = sender.address().ok_or("user_id can't get1")?;
        let user_name = sender.name().ok_or("user_name can't get1")?.to_string();
        let message = message.body_text(0).ok_or("message can't get")?.to_string();
        if user_id == self_id {
            return Ok(());
        }
        let  event_json = serde_json::json!({
            "time":SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
            "self_id":self_id,
            "post_type":"message",
            "message_type":"private",
            "sub_type":"friend",
            "message_id":Uuid::new_v4().to_string(),
            "user_id":user_id,
            "message":message,
            "raw_message":message,
            "font":0,
            "sender":{
                "user_id":user_id,
                "nickname":user_name,
            },
            "platform":"email"
        });
        RT_PTR.spawn_blocking(move ||{
            let json_str = event_json.to_string();
            cq_add_log(&format!("EMAIL_OB_EVENT:{json_str}")).unwrap();
            if let Err(e) = crate::cqevent::do_1207_event(&json_str) {
                crate::cqapi::cq_add_log(format!("{:?}", e).as_str()).unwrap();
            }
        });
        return Ok(());
    }

    fn do_connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let config_json_str = self.url.get(8..).ok_or("email url格式错误")?;
        let config_json:serde_json::Value =  serde_json::from_str(config_json_str)?;
        let imap_server = config_json.get("imap_server").ok_or("email url格式错误:没有 imap_server")?.as_str().ok_or("email url格式错误: imap_server 不是字符串")?.to_owned();
        let imap_port = config_json["imap_port"].as_u64().ok_or("email url格式错误: imap_port 不是数字")?;
        let imap_ssl = config_json["imap_ssl"].as_bool().ok_or("email url格式错误: imap_ssl 不是布尔值")?;
        let username = config_json["username"].as_str().ok_or("email url格式错误: username 不是字符串")?.to_owned();
        let password = config_json["password"].as_str().ok_or("email url格式错误: password 不是字符串")?.to_owned();
        let smtp_server = config_json["smtp_server"].as_str().ok_or("email url格式错误: smtp_server 不是字符串")?.to_owned();
        let smtp_port = config_json["smtp_port"].as_u64().ok_or("email url格式错误: smtp_port 不是数字")?;
        *self.smtp_server.write().unwrap() = smtp_server;
        *self.smtp_port.write().unwrap() = smtp_port as u16;
        *self.self_id.write().unwrap() = username.to_owned();
        *self.password.write().unwrap() = password.to_owned();
        if imap_ssl {
            let tls = native_tls::TlsConnector::builder().build()?;
            let client = imap::connect((imap_server.to_owned(), imap_port as u16), imap_server.to_owned(), &tls)?;
            let mut imap_session: imap::Session<native_tls::TlsStream<TcpStream>> = client
                .login(username, password)
                .map_err(|e| e.0)?;
            imap_session.select("INBOX")?;
            cq_add_log(&format!("邮件协议已经连接:{}",self.url)).unwrap();
            loop {
                if self.is_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    imap_session.logout()?;
                    break;
                }
                let uids = imap_session.search("NEW")?;
                if uids.is_empty() {
                    let handle = imap_session.idle()?;
                    handle.wait_with_timeout(Duration::from_secs(5))?;
                    continue;
                }else {
                    for uid in uids {
                        let fetcharr = imap_session.fetch(uid.to_string(), "RFC822")?;
                        self.deal_fetcharr(&fetcharr)?;
                        imap_session.store(uid.to_string(), "+FLAGS (\\Seen)")?;
                    }
                }
            }
        }else {
            let stream = TcpStream::connect(format!("{imap_server}:{imap_port}"))?;
            let client = imap::Client::new(stream);
            let mut imap_session = client
                .login(username, password)
                .map_err(|e| e.0)?;
            imap_session.select("INBOX")?;
            cq_add_log(&format!("邮件协议已经连接:{}",self.url)).unwrap();
            loop {
                if self.is_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    imap_session.logout()?;
                    break;
                }
                let uids = imap_session.search("NEW")?;
                if uids.is_empty() {
                    let handle = imap_session.idle()?;
                    handle.wait_with_timeout(Duration::from_secs(5))?;
                    continue;
                }else {
                    for uid in uids {
                        let fetcharr = imap_session.fetch(uid.to_string(), "RFC822")?;
                        self.deal_fetcharr(&fetcharr)?;
                        imap_session.store(uid.to_string(), "+FLAGS (\\Seen)")?;
                    }
                }
            }
        }
        Ok(())
    }
    fn get_json_bool(js:&serde_json::Value,key:&str) -> bool {
        if let Some(j) = js.get(key) {
            if j.is_boolean() {
                return j.as_bool().unwrap();
            } else if j.is_string(){
                if j.as_str().unwrap() == "true" {
                    return true;
                } else {
                    return false;
                }
            }
            else {
                return false;
            }
        } else {
            return false;
        }
    }
    fn get_auto_escape_from_params(&self,params:&serde_json::Value) -> bool {
        let is_auto_escape = Self::get_json_bool(params, "auto_escape");
        return is_auto_escape;
    }
    async fn make_email_msg(&self,message_arr:&serde_json::Value) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut to_ret = "".to_owned();
        for it in message_arr.as_array().ok_or("message not arr")? {
            let tp = it.get("type").ok_or("type not found")?;
            if tp == "text"{
                let t = it.get("data").ok_or("data not found")?.get("text").ok_or("text not found")?.as_str().ok_or("text not str")?.to_owned();
                to_ret.push_str(&t);
            }
        }
        Ok(to_ret)
    }
    async fn deal_ob_send_private_msg(&self,params:&serde_json::Value,echo:&serde_json::Value) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let message_arr:serde_json::Value;
        let message_rst = params.get("message").ok_or("message not found")?;
        if message_rst.is_string() {
            if self.get_auto_escape_from_params(&params) {
                message_arr = serde_json::json!(
                    [{"type":"text","data":{
                        "text": message_rst.as_str()
                    }}]
                );
            } else {
                message_arr = str_msg_to_arr(message_rst).map_err(|x|{
                    format!("str_msg_to_arr err:{:?}",x)
                })?;
            }
        }else {
            message_arr = params.get("message").ok_or("message not found")?.to_owned();
        }
       
        let user_id = read_json_str(params,"user_id");
        let bot_id = self.self_id.read().unwrap().to_owned();
        let email_msg = self.make_email_msg(&message_arr).await?;
        let server = self.smtp_server.read().unwrap().to_owned();
        let port = self.smtp_port.read().unwrap().to_owned();
        let password = self.password.read().unwrap().to_owned();
        let email = lettre::Message::builder()
            .from(format!("Bot <{bot_id}>").parse()?)
            .to(format!("User <{user_id}>").parse()?)
            .subject("Dear User")
            .header(lettre::message::header::ContentType::TEXT_PLAIN)
            .body(email_msg)?;
        let creds = lettre::transport::smtp::authentication::Credentials::new(bot_id, password);
        let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(server.to_owned()).port(port)
            .tls(lettre::transport::smtp::client::Tls::Required(lettre::transport::smtp::client::TlsParameters::new(server)?))
            .credentials(creds)
            .build();
        let send_ret = match mailer.send(email).await {
            Ok(_) => "ok".to_owned(),
            Err(e) => format!("{:?}", e),
        };
        if send_ret == "ok" {
            return Ok(serde_json::json!({
                "status": "ok",
                "retcode": 0,
                "data": {
                    "message_id": Uuid::new_v4().to_string()
                },
                "echo": echo.to_owned()
            }));
        }else {
            return Ok(serde_json::json!({
                "status": "failed",
                "retcode": 1404,
                "data": {
                },
                "message":send_ret,
                "echo": echo.to_owned()
            }));
        }
    }
}

#[async_trait]
impl BotConnectTrait for EmailConnect {

    async fn disconnect(&mut self){
        self.is_stop.store(true,std::sync::atomic::Ordering::Relaxed);
    }

    fn get_alive(&self) -> bool {
        return !self.is_stop.load(std::sync::atomic::Ordering::Relaxed);
    }

    async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let self_t = self.clone();
        thread::spawn(move ||{
            if let Err(e) = self_t.do_connect() {
                crate::cqapi::cq_add_log(format!("{:?}", e).as_str()).unwrap();
            }
            self_t.is_stop.store(true,std::sync::atomic::Ordering::Relaxed);
        });
        Ok(())
    }

    
    
    async fn call_api(&self,_platform:&str,_self_id:&str,_passive_id:&str,json:&mut serde_json::Value) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {

       let action = json.get("action").ok_or("action not found")?.as_str().ok_or("action not str")?;
        let echo = json.get("echo").unwrap_or(&serde_json::Value::Null);
        let def = serde_json::json!({});
        let params = json.get("params").unwrap_or(&def);
        
        let send_json = match action {
            "send_private_msg" => {
                // cq_add_log("send_private_msg触发").unwrap();
                self.deal_ob_send_private_msg(&params,&echo).await?
            },
            _ => {
                serde_json::json!({
                    "status":"failed",
                    "retcode":1404,
                    "echo":echo
                })
            }
        };
        return Ok(send_json);
    }

    fn get_platform_and_self_id(&self) -> Vec<(String,String)> {
        let lk = self.self_id.read().unwrap();
        if lk.is_empty() {
            return vec![];
        }
        return vec![("email".to_owned(),lk.to_owned())];
    }
}
