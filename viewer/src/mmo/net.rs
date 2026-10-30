//! MarsScape WebSocket client. Native: tungstenite on a background thread, talking to the game
//! through channels. Browser: `web_sys::WebSocket`, its callbacks queue events in a thread-local
//! (JS objects can't live in a Bevy resource). Both give the same `Ws::send` / `Ws::poll`.

use super::proto::{C2S, S2C};

pub enum NetEvent {
    Open,
    Msg(S2C),
    Closed(String),
}

fn decode(text: &str) -> Option<S2C> {
    serde_json::from_str(text).ok()
}

/// `ws://host/` + `room/<site>`; accepts a base URL with or without a trailing slash or `/room`.
pub fn room_url(base: &str, site: &str) -> String {
    let b = base.trim_end_matches('/');
    let b = b.strip_suffix("/room").unwrap_or(b);
    format!("{b}/room/{}", super::proto::sanitize_site(site))
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::*;
    use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
    use std::time::Duration;
    use tungstenite::{Message, stream::MaybeTlsStream};

    pub struct Ws {
        tx: Sender<String>,
        rx: Receiver<NetEvent>,
    }

    // the receiver is only touched from one system at a time
    unsafe impl Sync for Ws {}

    impl Ws {
        pub fn connect(url: &str) -> Ws {
            let (tx, out_rx) = channel::<String>();
            let (ev_tx, rx) = channel::<NetEvent>();
            let url = url.to_string();
            std::thread::Builder::new()
                .name("marsscape-ws".into())
                .spawn(move || {
                    let (mut sock, _) = match tungstenite::connect(url.as_str()) {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = ev_tx.send(NetEvent::Closed(e.to_string()));
                            return;
                        }
                    };
                    let timeout = Some(Duration::from_millis(30));
                    match sock.get_mut() {
                        MaybeTlsStream::Plain(s) => {
                            let _ = s.set_read_timeout(timeout);
                        }
                        MaybeTlsStream::Rustls(s) => {
                            let _ = s.get_mut().set_read_timeout(timeout);
                        }
                        _ => {}
                    }
                    let _ = ev_tx.send(NetEvent::Open);
                    loop {
                        loop {
                            match out_rx.try_recv() {
                                Ok(s) => {
                                    if let Err(e) = sock.send(Message::text(s)) {
                                        let _ = ev_tx.send(NetEvent::Closed(e.to_string()));
                                        return;
                                    }
                                }
                                Err(TryRecvError::Empty) => break,
                                Err(TryRecvError::Disconnected) => {
                                    let _ = sock.close(None);
                                    let _ = sock.flush();
                                    return;
                                }
                            }
                        }
                        match sock.read() {
                            Ok(Message::Text(t)) => {
                                if let Some(m) = decode(t.as_str()) {
                                    if ev_tx.send(NetEvent::Msg(m)).is_err() {
                                        return;
                                    }
                                }
                            }
                            Ok(Message::Close(_)) => {
                                let _ = ev_tx.send(NetEvent::Closed("closed by the server".into()));
                                return;
                            }
                            Ok(_) => {}
                            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                            Err(e) => {
                                let _ = ev_tx.send(NetEvent::Closed(e.to_string()));
                                return;
                            }
                        }
                    }
                })
                .expect("spawn websocket thread");
            Ws { tx, rx }
        }

        pub fn send(&self, m: &C2S) {
            if let Ok(s) = serde_json::to_string(m) {
                let _ = self.tx.send(s);
            }
        }

        pub fn poll(&mut self) -> Vec<NetEvent> {
            let mut v = Vec::new();
            loop {
                match self.rx.try_recv() {
                    Ok(e) => v.push(e),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        v.push(NetEvent::Closed("connection thread ended".into()));
                        break;
                    }
                }
            }
            v
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::rc::Rc;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{CloseEvent, Event, MessageEvent, WebSocket};

    struct Sock {
        ws: WebSocket,
        q: Rc<RefCell<VecDeque<NetEvent>>>,
        _cbs: Vec<Box<dyn std::any::Any>>,
    }

    thread_local! {
        static SOCKS: RefCell<HashMap<u32, Sock>> = RefCell::new(HashMap::new());
        static NEXT: RefCell<u32> = const { RefCell::new(1) };
    }

    pub struct Ws {
        id: u32,
        failed: Option<String>,
    }

    impl Ws {
        pub fn connect(url: &str) -> Ws {
            let id = NEXT.with(|n| {
                let mut n = n.borrow_mut();
                *n += 1;
                *n
            });
            let q: Rc<RefCell<VecDeque<NetEvent>>> = Rc::new(RefCell::new(VecDeque::new()));
            match WebSocket::new(url) {
                Ok(ws) => {
                    let q1 = q.clone();
                    let on_open = Closure::<dyn FnMut(Event)>::new(move |_| q1.borrow_mut().push_back(NetEvent::Open));
                    let q2 = q.clone();
                    let on_msg = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
                        if let Some(t) = e.data().as_string() {
                            if let Some(m) = decode(&t) {
                                q2.borrow_mut().push_back(NetEvent::Msg(m));
                            }
                        }
                    });
                    let q3 = q.clone();
                    let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |e: CloseEvent| {
                        q3.borrow_mut().push_back(NetEvent::Closed(format!("closed ({})", e.code())));
                    });
                    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                    ws.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
                    ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
                    let cbs: Vec<Box<dyn std::any::Any>> = vec![Box::new(on_open), Box::new(on_msg), Box::new(on_close)];
                    SOCKS.with(|s| s.borrow_mut().insert(id, Sock { ws, q, _cbs: cbs }));
                }
                Err(e) => return Ws { id, failed: Some(format!("{e:?}")) },
            }
            Ws { id, failed: None }
        }

        pub fn send(&self, m: &C2S) {
            let Ok(s) = serde_json::to_string(m) else { return };
            SOCKS.with(|socks| {
                if let Some(k) = socks.borrow().get(&self.id) {
                    if k.ws.ready_state() == WebSocket::OPEN {
                        let _ = k.ws.send_with_str(&s);
                    }
                }
            });
        }

        pub fn poll(&mut self) -> Vec<NetEvent> {
            if let Some(f) = self.failed.take() {
                return vec![NetEvent::Closed(f)];
            }
            SOCKS.with(|socks| socks.borrow().get(&self.id).map_or(Vec::new(), |k| k.q.borrow_mut().drain(..).collect()))
        }
    }

    impl Drop for Ws {
        fn drop(&mut self) {
            SOCKS.with(|socks| {
                if let Some(k) = socks.borrow_mut().remove(&self.id) {
                    k.ws.set_onclose(None);
                    k.ws.set_onmessage(None);
                    k.ws.set_onopen(None);
                    let _ = k.ws.close();
                }
            });
        }
    }
}

pub use imp::Ws;
