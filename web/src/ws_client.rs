// A minimal WebSocket request/response client for the browser. Wraps
// web_sys::WebSocket's callback-based API in an async `call()`, so the rest
// of the app can `.await` a response instead of juggling closures.
//
// Supports genuinely concurrent requests (server-api dispatches each one
// on its own task and doesn't guarantee ordering back — see its README),
// routed by `request_id`: each `call()` picks a fresh id, and `onmessage`
// peeks just that field out of the incoming `Response` to find the right
// waiting sender. This isn't optional polish — the app fires off several
// concurrent `download_file` calls per view (one per photo) on the same
// connection, and a single-slot client would silently hand one request's
// bytes to a completely different caller.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use lestobackupper_schema::protocol::ResponseRef;
use futures_channel::oneshot;
use js_sys::{ArrayBuffer, Uint8Array};
use planus::ReadAsRoot;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{BinaryType, ErrorEvent, MessageEvent, WebSocket};

type Pending = Rc<RefCell<HashMap<u64, oneshot::Sender<Vec<u8>>>>>;

pub struct WsClient {
    socket: WebSocket,
    pending: Pending,
    next_request_id: Rc<Cell<u64>>,
    // Kept alive for as long as the client is: dropping a Closure detaches
    // its JS-side callback.
    _onmessage: Closure<dyn FnMut(MessageEvent)>,
    _onerror: Closure<dyn FnMut(ErrorEvent)>,
}

impl WsClient {
    pub async fn connect(url: &str) -> Result<Self, JsValue> {
        let socket = WebSocket::new(url)?;
        socket.set_binary_type(BinaryType::Arraybuffer);

        let pending: Pending = Rc::new(RefCell::new(HashMap::new()));

        let onmessage_pending = pending.clone();
        let onmessage = Closure::wrap(Box::new(move |ev: MessageEvent| {
            let Ok(buf) = ev.data().dyn_into::<ArrayBuffer>() else { return };
            let bytes = Uint8Array::new(&buf).to_vec();
            let Some(request_id) = peek_request_id(&bytes) else { return };
            if let Some(tx) = onmessage_pending.borrow_mut().remove(&request_id) {
                let _ = tx.send(bytes);
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        let onerror_pending = pending.clone();
        let onerror = Closure::wrap(Box::new(move |_: ErrorEvent| {
            // Drops every waiting sender, so anyone mid-`call()` gets a
            // clean "connection closed" error instead of hanging forever.
            onerror_pending.borrow_mut().clear();
        }) as Box<dyn FnMut(ErrorEvent)>);
        socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));

        let (open_tx, open_rx) = oneshot::channel::<()>();
        let open_tx = Rc::new(RefCell::new(Some(open_tx)));
        let onopen = Closure::once(move |_: web_sys::Event| {
            if let Some(tx) = open_tx.borrow_mut().take() {
                let _ = tx.send(());
            }
        });
        socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget(); // fires at most once; nothing to clean up after

        open_rx.await.map_err(|_| JsValue::from_str("socket closed before it finished opening"))?;

        Ok(Self { socket, pending, next_request_id: Rc::new(Cell::new(1)), _onmessage: onmessage, _onerror: onerror })
    }

    /// `bytes` must already encode a `Request` whose `request_id` is the
    /// value this returned — callers build the request themselves (see
    /// `api::call`), so `WsClient` doesn't need to know about the wire
    /// schema's request side, only enough of the response side to route.
    pub fn next_request_id(&self) -> u64 {
        let id = self.next_request_id.get();
        self.next_request_id.set(id + 1);
        id
    }

    pub async fn call(&self, request_id: u64, bytes: &[u8]) -> Result<Vec<u8>, JsValue> {
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().insert(request_id, tx);
        if let Err(e) = self.socket.send_with_u8_array(bytes) {
            self.pending.borrow_mut().remove(&request_id);
            return Err(e);
        }
        rx.await.map_err(|_| JsValue::from_str("connection closed while waiting for a response"))
    }
}

fn peek_request_id(bytes: &[u8]) -> Option<u64> {
    ResponseRef::read_as_root(bytes).ok()?.request_id().ok()
}
