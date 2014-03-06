//
// zhtta.rs
//
// Starting code for PS3
// Running on Rust 0.9
//
// Note that this code has serious security risks!  You should not run it 
// on any system with access to sensitive files.
// 
// University of Virginia - cs4414 Spring 2014
// Weilin Xu and David Evans
// Version 0.5

// To see debug! outputs set the RUST_LOG environment variable, e.g.: export RUST_LOG="zhtta=debug"

#[feature(globs)];
extern mod extra;

use std::io::*;
use std::io::net::ip::{SocketAddr};
use std::{os, str, libc, from_str};
use std::path::Path;
use std::hashmap::HashMap;

use extra::getopts;
use extra::arc::MutexArc;
use extra::arc::RWArc;
use extra::lru_cache::LruCache;

use std::run::{Process, ProcessOptions};

static SERVER_NAME : &'static str = "Zhtta Version 0.5";

static IP : &'static str = "127.0.0.1";
static PORT : uint = 4414;
static WWW_DIR : &'static str = "./www";

static HTTP_OK : &'static str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n";
static HTTP_BAD : &'static str = "HTTP/1.1 404 Not Found\r\n\r\n";

static COUNTER_STYLE : &'static str = "<doctype !html><html><head><title>Hello, Rust!</title>
             <style>body { background-color: #884414; color: #FFEEAA}
                    h1 { font-size:2cm; text-align: center; color: black; text-shadow: 0 0 4mm red }
                    h2 { font-size:2cm; text-align: center; color: black; text-shadow: 0 0 4mm green }
             </style></head>
             <body>";

static CACHE_SIZE : uint = 4;
static BLOCK_SIZE : uint = 4;
static FILE_SIZE_THRESHOLD : uint = 512;
static MAX_NUM_OF_TASKS : uint = 128;

struct HTTP_Request {
    // Use peer_name as the key to access TcpStream in hashmap. 

    // (Due to a bug in extra::arc in Rust 0.9, it is very inconvenient to use TcpStream without the "Freeze" bound.
    //  See issue: https://github.com/mozilla/rust/issues/12139)
    peer_name: ~str,
    path: ~Path,
}

struct WebServer {
    ip: ~str,
    port: uint,
    www_dir_path: ~Path,
    
    request_queue_arc: MutexArc<~[HTTP_Request]>,
    stream_map_arc: MutexArc<HashMap<~str, Option<std::io::net::tcp::TcpStream>>>,
    
    notify_port: Port<()>,
    shared_notify_chan: SharedChan<()>,

    counter_arc: RWArc<uint>,
    semophore_arc: RWArc<uint>,
    cache_arc: MutexArc<LruCache<Path, ~[u8]>>   // MutexArc required due to mentioned Freeze bug in Rust 0.9
}

impl WebServer {
    fn new(ip: &str, port: uint, www_dir: &str) -> WebServer {
        let (notify_port, shared_notify_chan) = SharedChan::new();
        let www_dir_path = ~Path::new(www_dir);
        os::change_dir(www_dir_path.clone());

        WebServer {
            ip: ip.to_owned(),
            port: port,
            www_dir_path: www_dir_path,
                        
            request_queue_arc: MutexArc::new(~[]),
            stream_map_arc: MutexArc::new(HashMap::new()),
            
            notify_port: notify_port,
            shared_notify_chan: shared_notify_chan,

            counter_arc: RWArc::new(0),
            semophore_arc: RWArc::new(MAX_NUM_OF_TASKS),
            cache_arc: MutexArc::new(LruCache::new(CACHE_SIZE))   
        }
    }
    
    fn run(&mut self) {
        self.listen();
        self.dequeue_static_file_request();
    }
    
    fn listen(&mut self) {
        let addr = from_str::<SocketAddr>(format!("{:s}:{:u}", self.ip, self.port)).expect("Address error.");
        let www_dir_path_str = self.www_dir_path.as_str().expect("invalid www path?").to_owned();
        
        let request_queue_arc = self.request_queue_arc.clone();
        let shared_notify_chan = self.shared_notify_chan.clone();
        let stream_map_arc = self.stream_map_arc.clone();

        // add cache_arc?
        let counter_arc = self.counter_arc.clone();
        let cache_arc = self.cache_arc.clone();

                
        spawn(proc() {
            let mut acceptor = net::tcp::TcpListener::bind(addr).listen();
            println!("{:s} listening on {:s} (serving from: {:s}).", 
                     SERVER_NAME, addr.to_str(), www_dir_path_str);
            
            for stream in acceptor.incoming() {
                let (queue_port, queue_chan) = Chan::new();
                queue_chan.send(request_queue_arc.clone());
                
                let notify_chan = shared_notify_chan.clone();
                let stream_map_arc = stream_map_arc.clone();

                let (arc_port, arc_chan) = Chan::new();
                arc_chan.send(counter_arc.clone());     // .clone isn't detected prior to complation, results in segfault later when trying to write to RWArc in process
                
                let (cache_port, cache_chan) = Chan::new();
                cache_chan.send(cache_arc.clone());
                // Spawn a task to handle the connection.
                spawn(proc() {

                    let local_counter = arc_port.recv();
                    local_counter.write( |num| {        // Segfault caused here (see above comment)
                        *num += 1;
                    });

                    let request_queue_arc = queue_port.recv();
                  
                    let mut stream = stream;
                    
                    let peer_name = WebServer::get_peer_name(&mut stream);
                    
                    let mut buf = [0, ..500];
                    stream.read(buf);
                    let request_str = str::from_utf8(buf);
                    debug!("Request:\n{:s}", request_str);
                    
                    let req_group : ~[&str]= request_str.splitn(' ', 3).collect();
                    if req_group.len() > 2 {
                        let path_str = "." + req_group[1].to_owned();
                        
                        let mut path_obj = ~os::getcwd();
                        path_obj.push(path_str.clone());
                        
                        let ext_str = match path_obj.extension_str() {
                            Some(e) => e,
                            None => "",
                        };
                        
                        debug!("Requested path: [{:s}]", path_obj.as_str().expect("error"));
                        debug!("Requested path: [{:s}]", path_str);
                             
                        if path_str == ~"./" {
                            debug!("===== Counter Page request =====");

                            WebServer::respond_with_counter_page(local_counter, stream);
                            debug!("=====Terminated connection from [{:s}].=====", peer_name);
                        } else if !path_obj.exists() || path_obj.is_dir() {
                            debug!("===== Error page request =====");
                            WebServer::respond_with_error_page(stream, path_obj);
                            debug!("=====Terminated connection from [{:s}].=====", peer_name);
                        } else if ext_str == "shtml" { // Dynamic web pages.
                            debug!("===== Dynamic Page request =====");
                            WebServer::respond_with_dynamic_page(stream, path_obj);
                            debug!("=====Terminated connection from [{:s}].=====", peer_name);
                        } else { 
                            debug!("===== Static Page request =====");
                            if path_obj.stat().size.to_uint().unwrap() < FILE_SIZE_THRESHOLD {
                                debug!("File size is under threshold. Immediately responding.");
                                let cache_arc = cache_port.recv();
                                WebServer::respond_with_static_file(stream, path_obj, cache_arc);
                            }
                            else {
                                debug!("File size is larger than the threshold. Enqueuing request.");
                                WebServer::enqueue_static_file_request(stream, path_obj, stream_map_arc, request_queue_arc, notify_chan);
                            }
                        }
                    }
                });
            }
        });
    }

    fn respond_with_error_page(stream: Option<std::io::net::tcp::TcpStream>, path: &Path) {
        let mut stream = stream;
        let msg: ~str = format!("Cannot open: {:s}", path.as_str().expect("invalid path").to_owned());

        stream.write(HTTP_BAD.as_bytes());
        stream.write(msg.as_bytes());
    }

    // TODO: Safe visitor counter. (Completed)
    fn respond_with_counter_page(given_arc: RWArc<uint>, stream: Option<std::io::net::tcp::TcpStream>) {
        let mut stream = stream;
        let arc = given_arc;
        let response: ~str = 
            format!("{:s}{:s}<h1>Greetings, Krusty!</h1>
                     <h2>Visitor count: {:u}</h2></body></html>\r\n", 
                    HTTP_OK, COUNTER_STYLE, 
                    arc.read(|count| { *count }) );
        debug!("Responding to counter request");
        stream.write(response.as_bytes());              
    }
    
    // TODO: Streaming file. (Completed)
    // TODO: Application-layer file caching (Completed)
    fn respond_with_static_file(stream: Option<std::io::net::tcp::TcpStream>, path: & Path, cache_arc: MutexArc<LruCache<Path, ~[u8]>>) {
        let mut stream = stream;
            cache_arc.access(|lru_cache| {
                let mut buffer_to_cache: ~[u8] = ~[];
                match lru_cache.get(path) {
                    Some(buffer)  => {
                        debug!("Found data in the cache!");
                        stream.write(HTTP_OK.as_bytes());
                        let buffer_size = buffer.len();
                        let mut index = 0;
                        while BLOCK_SIZE < (buffer_size - index) {
                            let bytes = buffer.slice(index, index+BLOCK_SIZE);
                            stream.write(bytes);
                            index += BLOCK_SIZE;
                            buffer_to_cache.push_all_move(bytes.to_owned());
                        }
                        let bytes = buffer.slice(index, buffer_size);
                        stream.write(bytes);
                        stream.flush();
                        buffer_to_cache.push_all_move(bytes.to_owned());
                    },
                    None      => {
                        debug!("File not in cache. Streaming from memory.");
                        let mut file_reader = File::open(path).expect("Invalid file!");
                        stream.write(HTTP_OK.as_bytes());
                        let buffer_size = path.stat().size.to_uint().unwrap();
                        let mut index = 0;
                        while BLOCK_SIZE < (buffer_size - index) {
                            let bytes = file_reader.read_bytes(BLOCK_SIZE);
                            stream.write(bytes);
                            index += BLOCK_SIZE;
                            buffer_to_cache.push_all_move(bytes);
                        }
                        let bytes = file_reader.read_to_end();
                        stream.write(bytes);
                        stream.flush();
                        buffer_to_cache.push_all_move(bytes);
                    }
                }
                lru_cache.put(path.clone(), buffer_to_cache);
                debug!("File data now cached");
            });
    }
    
    // TODO: Server-side gashing. (Completed)
    fn respond_with_dynamic_page(stream: Option<std::io::net::tcp::TcpStream>, path: &Path) {
        let mut stream = stream;
        let mut file_reader = File::open(path).expect("Invalid file!");
        let contents = file_reader.read_to_str();

        let mut output = contents.clone();
        let tags: ~[&str] = contents.split_str("<").collect();
        for i in range(0, tags.len()) {
            if tags[i].contains("#exec") {
                let exec_tag: ~[&str] = tags[i].split_terminator('"').collect();
                let cmd = exec_tag[1];

                let (g_port, g_chan): (Port<~str>, Chan<~str>) = Chan::new();
                let (cmd_port, cmd_chan): (Port<~str>, Chan<~str>) = Chan::new();
                cmd_chan.send(cmd.to_owned());

                spawn(proc (){
                    let gash = Process::new( "../gash", &[~""], ProcessOptions::new());

                    match gash {
                        Some (mut g) => {                             
                            {
                                let gash = &mut g;
                                let gash_in = gash.input();
                                let cmd = cmd_port.recv();
                                let bytes_in = cmd.as_bytes();
                                debug!("Port recieved: {}\n\tAs bytes {}", cmd, bytes_in.to_str());
                                gash_in.write(bytes_in);
                            }
                            g.close_input();
                            {
                                let gash = &mut g;
                                let gash_out = gash.output();
                                let result = gash_out.read_to_str();

                                debug!("Unformated output written as {}", result);
                                let str_out = result.replace("[www] gash > ","").replace("\n", "");

                                debug!("Formatted output written as {}", str_out);
                                g_chan.send(str_out);
                            }
                            g.close_outputs();
                            g.finish();
                        },
                        None         => { println("Process was not created;"); }
                    }
                });

                let gash_out = g_port.recv();
                let mut tag = ~"<!--#exec cmd=\"";
                tag = tag + cmd;
                tag = tag + "\" -->";
                output = contents.replace(tag, gash_out);
            }

        }
        stream.write(HTTP_OK.as_bytes());
        stream.write(output.as_bytes());
    }
    
    // TODO: Smarter Scheduling.
    // WahooFirst scheduling (Completed)
    fn enqueue_static_file_request(stream: Option<std::io::net::tcp::TcpStream>, path_obj: &Path, stream_map_arc: MutexArc<HashMap<~str, Option<std::io::net::tcp::TcpStream>>>, req_queue_arc: MutexArc<~[HTTP_Request]>, notify_chan: SharedChan<()>) {
        // Save stream in hashmap for later response.
        let mut stream = stream;
        let peer_name = WebServer::get_peer_name(&mut stream);
        let ip_addr = WebServer::get_ip_address(&mut stream);
        let (stream_port, stream_chan) = Chan::new();
        stream_chan.send(stream);
        unsafe {
            // Use an unsafe method, because TcpStream in Rust 0.9 doesn't have "Freeze" bound.
            stream_map_arc.unsafe_access(|local_stream_map| {
                let stream = stream_port.recv();
                local_stream_map.swap(peer_name.clone(), stream);
            });
        }
        
        // Enqueue the HTTP request.
        let req = HTTP_Request { peer_name: peer_name.clone(), path: ~path_obj.clone() };
        let (req_port, req_chan) = Chan::new();
        req_chan.send(req);


        debug!("Waiting for queue mutex lock.");
        req_queue_arc.access(|local_req_queue| {
            debug!("Got queue mutex lock.");
            let req: HTTP_Request = req_port.recv();

            // If local, insert into vector at index 0 (Give it highest priority)
            if ip_addr.starts_with("128.143.") {
                local_req_queue.insert(0, req);
            }
            else if ip_addr.starts_with("137.54.") {
                local_req_queue.insert(0, req);
            }
            else {
                // Else push onto "queue"
                local_req_queue.push(req);  // "Not actually a queue, but I'mma call it a queue" queue (actually a vector)
                debug!("A new request enqueued, now the length of queue is {:u}.", local_req_queue.len());
            } 
        });
        
        notify_chan.send(()); // Send incoming notification to responder task.
    }
    
    // TODO: Smarter Scheduling.
    // Multiple Response Tasks (Completed)
    fn dequeue_static_file_request(&mut self) {

        let req_queue_get = self.request_queue_arc.clone();
        let stream_map_get = self.stream_map_arc.clone();
        let semophore_arc_get = self.semophore_arc.clone();
        
        // Port<> cannot be sent to another task. So we have to make this task as the main task that can access self.notify_port.
        
        let (request_port, request_chan) = Chan::new();

        loop {
            self.notify_port.recv();    // waiting for new request enqueued.
            
            req_queue_get.access( |req_queue| {
                match req_queue.shift_opt() { // FIFO queue.
                    None => { /* do nothing */ }
                    Some(req) => {
                        request_chan.send(req);
                        debug!("A new request dequeued, now the length of queue is {:u}.", req_queue.len());
                    }
                }
            });
            
            let request = request_port.recv();
            
            // Get stream from hashmap.
            // Use unsafe method, because TcpStream in Rust 0.9 doesn't have "Freeze" bound.
            let (stream_port, stream_chan) = Chan::new();
            unsafe {
                stream_map_get.unsafe_access(|local_stream_map| {
                    let stream = local_stream_map.pop(&request.peer_name).expect("no option tcpstream");
                    stream_chan.send(stream);
                });
            }
            
            // TODO: Spawning more tasks to respond the dequeued requests concurrently. You may need a semophore to control the concurrency.
            let mut spaces_available = 0;
            semophore_arc_get.read(|num| { spaces_available = *num; });
            if spaces_available > 0 {
                let semophore_arc = semophore_arc_get.clone();
                let (cache_port, cache_chan) = Chan::new();
                let cache_arc_get = self.cache_arc.clone();
                cache_chan.send(cache_arc_get);
                spawn(proc(){
                    semophore_arc.write(|num| { *num -= 1; });
                    let stream = stream_port.recv();
                    let cache_arc = cache_port.recv();              
                    WebServer::respond_with_static_file(stream, request.path, cache_arc);
                    debug!("=====Terminated connection from [{:s}].=====", request.peer_name);
                    // Close stream automatically.
                    semophore_arc.write(|num| { *num += 1; });
                });
            }

                
        }
    }
    
    fn get_peer_name(stream: &mut Option<std::io::net::tcp::TcpStream>) -> ~str {
        match *stream {
            Some(ref mut s) => {
                         match s.peer_name() {
                            Some(pn) => {pn.to_str()},
                            None => (~"")
                         }
                       },
            None => (~"")
        }
    }

    fn get_ip_address(stream: &mut Option<std::io::net::tcp::TcpStream>) -> ~str {
        match *stream {
            Some (ref mut s) => {
                match s.peer_name() {
                    Some(pn) => {pn.ip.to_str()},
                    None => (~"")
                }
            },
            None => (~"")
        }
    }
}

fn get_args() -> (~str, uint, ~str) {
    fn print_usage(program: &str) {
        println!("Usage: {:s} [options]", program);
        println!("--ip     \tIP address, \"{:s}\" by default.", IP);
        println!("--port   \tport number, \"{:u}\" by default.", PORT);
        println!("--www    \tworking directory, \"{:s}\" by default", WWW_DIR);
        println("-h --help \tUsage");
    }
    
    /* Begin processing program arguments and initiate the parameters. */
    let args = os::args();
    let program = args[0].clone();
    
    let opts = ~[
        getopts::optopt("ip"),
        getopts::optopt("port"),
        getopts::optopt("www"),
        getopts::optflag("h"),
        getopts::optflag("help")
    ];

    let matches = match getopts::getopts(args.tail(), opts) {
        Ok(m) => { m }
        Err(f) => { fail!(f.to_err_msg()) }
    };

    if matches.opt_present("h") || matches.opt_present("help") {
        print_usage(program);
        unsafe { libc::exit(1); }
    }
    
    let ip_str = if matches.opt_present("ip") {
                    matches.opt_str("ip").expect("invalid ip address?").to_owned()
                 } else {
                    IP.to_owned()
                 };
    
    let port:uint = if matches.opt_present("port") {
                        from_str::from_str(matches.opt_str("port").expect("invalid port number?")).expect("not uint?")
                    } else {
                        PORT
                    };
    
    let www_dir_str = if matches.opt_present("www") {
                        matches.opt_str("www").expect("invalid www argument?") 
                      } else { WWW_DIR.to_owned() };
    
    (ip_str, port, www_dir_str)
}

fn main() {
    let (ip_str, port, www_dir_str) = get_args();
    let mut zhtta = WebServer::new(ip_str, port, www_dir_str);
    zhtta.run();
}
