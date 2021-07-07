use std::ffi::OsString;
use std::net::{AddrParseError, SocketAddr, SocketAddrV4, SocketAddrV6};

use hyper::{Body, Error, Request, Response};
use hyper::server::conn::AddrStream;
use hyper::service::{make_service_fn, service_fn};
use structopt::StructOpt;

use wasp_sdk::proto::{Message, MessageType};

use crate::instance::{self, instance_ref, INSTANCES_COUNT};

#[derive(StructOpt, Debug, Clone)]
pub struct ServeOpt {
    pub(crate) addr: String,
    pub(crate) command: String,
    /// WASI pre-opened directory
    #[structopt(long = "dir", multiple = true, group = "wasi")]
    pub(crate) pre_opened_directories: Vec<String>,
    /// Application arguments
    #[structopt(multiple = true, parse(from_os_str))]
    pub(crate) args: Vec<OsString>,
}

impl ServeOpt {
    pub(crate) fn parse_addr(&self) -> Result<SocketAddr, AddrParseError> {
        let mut addr = self.addr.parse::<SocketAddrV4>()
                           .and_then(|a| Ok(SocketAddr::V4(a)));
        if addr.is_err() {
            addr = self.addr.parse::<SocketAddrV6>()
                       .and_then(|a| Ok(SocketAddr::V6(a)));
        }
        addr
    }
    pub(crate) fn get_name(&self) -> &String {
        &self.command
    }
    pub(crate) fn get_wasm_path(&self) -> &String {
        &self.command
    }
    pub(crate) fn get_preopen_dirs(&self) -> &Vec<String> {
        &self.pre_opened_directories
    }
    pub(crate) fn to_args_unchecked(&self) -> impl IntoIterator<Item=&str> {
        self.args.iter().map(|v| v.to_str().unwrap()).collect::<Vec<&str>>()
    }
}

static mut SERVER: Server = Server::new();

pub(crate) async fn serve(serve_options: ServeOpt) -> Result<(), anyhow::Error> {
    Server::serve(serve_options)
        .await
        .map_err(|e| anyhow::Error::msg(format!("{}", e)))
}

pub(crate) struct Server {}

impl Server {
    pub(crate) const fn new() -> Self {
        Server {}
    }

    async fn serve(serve_options: ServeOpt) -> Result<(), Box<dyn std::error::Error>> {
        let addr = serve_options.parse_addr()?;
        instance::rebuild(&serve_options).await?;
        // The closure inside `make_service_fn` is run for each connection,
        // creating a 'service' to handle requests for that specific connection.
        let make_service = make_service_fn(|socket: &AddrStream| {
            let _remote_addr = socket.remote_addr();
            async {
                // This is the `Service` that will handle the connection.
                // `service_fn` is a helper to convert a function that
                // returns a Response into a `Service`.
                Ok::<_, Error>(service_fn(|req| async {
                    let r = unsafe { &SERVER }.handle(req).await;
                    if let Err(ref e) = r {
                        eprintln!("{}", e)
                    }
                    r
                }))
            }
        });
        let srv = hyper::Server::bind(&addr).serve(make_service);
        println!("Listening on http://{}", addr);
        if let Err(e) = srv.await {
            eprintln!("SERVER error: {}", e);
        }
        Ok(())
    }

    async fn handle(&self, req: Request<Body>) -> Result<Response<Body>, String> {
        // return Ok(Response::default());
        let call_msg = req_to_call_msg(req).await;

        let thread_id = current_thread_id() % INSTANCES_COUNT;
        let ins = instance_ref(thread_id);
        let ctx_id = ins.gen_ctx_id();

        // println!("========= thread_id={}, ctx_id={}", thread_id, ctx_id);

        let data = serde_json::to_vec(&call_msg).or_else(|e| Err(format!("{}", e)))?;
        ins.call_guest_handler(thread_id as i32, ctx_id, ins.set_guest_request(ctx_id, data));
        let reply_msg: Message = serde_json::from_slice(
            ins
                .get_guest_response(ctx_id)
                .as_slice()
        ).unwrap();
        // println!("========= reply_msg={:?}", reply_msg);
        Ok(msg_to_resp(reply_msg))
    }
}

fn current_thread_id() -> usize {
    let thread_id: usize = format!("{:?}", ::std::thread::current().id())
        .matches(char::is_numeric)
        .collect::<Vec<&str>>()
        .join("")
        .parse().unwrap();
    return thread_id;
}

fn msg_to_resp(msg: Message) -> Response<Body> {
    let mut resp = Response::builder();
    for x in msg.headers.iter() {
        resp = resp.header(x.0, x.1);
    }
    match msg.mtype {
        MessageType::Reply => {
            resp = resp.status(200);
        },
        _ => {
            resp = resp.status(
                msg.headers
                   .get("status")
                   .unwrap_or(&"200".to_string())
                   .parse::<u16>().unwrap_or(200));
        }
    }
    resp.body(Body::from(msg.body)).unwrap()
}

async fn req_to_call_msg(req: Request<Body>) -> Message {
    let mut msg = Message::new(req.uri().to_string(), MessageType::Call, rand::random());
    let (parts, body) = req.into_parts();
    let body = hyper::body::to_bytes(body).await.map_or_else(|_| vec![], |v| v.to_vec());
    for x in parts.headers.iter() {
        msg = msg.set_header(
            x.0.to_string(),
            x.1
             .to_str()
             .map_or_else(|_| String::new(), |s| s.to_string()),
        );
    }
    msg.set_body(body)
}
