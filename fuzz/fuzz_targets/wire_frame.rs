//! What a stranger can put on a stream: the request frame, then the A2A message inside it, then the
//! Playbox operation inside that. Nothing here may panic, whatever the bytes.
#![no_main]
use a2a::{Message, SendMessageRequest};
use libfuzzer_sys::fuzz_target;
use playbox_net::wire::{Frame, Op};

fuzz_target!(|data: &[u8]| {
    let Ok(frame) = serde_json::from_slice::<Frame>(data) else { return };
    let _ = frame.grant();
    if let Some(params) = frame.rpc.params.clone() {
        if let Ok(req) = serde_json::from_value::<SendMessageRequest>(params.clone()) {
            if let Ok(Some(op)) = Op::from_message(&req.message) {
                let _ = op.name();
                let _ = op.needs();
            }
        }
        let _ = serde_json::from_value::<Message>(params);
    }
});
