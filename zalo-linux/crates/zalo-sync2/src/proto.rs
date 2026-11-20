//! The `Sync2.Message` and `Sync2.Conversation` protobuf records.
//!
//! Field numbers come from the `deserializeBinaryFromReader` functions in
//! Zalo PC's `shared-worker.*.js`. Unknown fields are skipped.

use crate::Sync2Error;

enum Wire<'a> {
    Varint(u64),
    Len(&'a [u8]),
    Fixed,
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn varint(&mut self) -> Result<u64, Sync2Error> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .buf
                .get(self.at)
                .ok_or_else(|| Sync2Error::Format("truncated varint".into()))?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err(Sync2Error::Format("varint too long".into()))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Sync2Error> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.buf.len());
        let end = end.ok_or_else(|| Sync2Error::Format("truncated field".into()))?;
        let out = &self.buf[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn next(&mut self) -> Result<Option<(u32, Wire<'a>)>, Sync2Error> {
        if self.at >= self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let wire = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Wire::Fixed
            }
            2 => {
                let len = self.varint()? as usize;
                Wire::Len(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Wire::Fixed
            }
            other => return Err(Sync2Error::Format(format!("unsupported wire type {other}"))),
        };
        Ok(Some((field, wire)))
    }
}

fn string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

macro_rules! decode_fields {
    ($buf:expr, $out:ident, { $($n:literal => $kind:ident $field:tt),* $(,)? }) => {{
        let mut r = Reader::new($buf);
        while let Some((field, wire)) = r.next()? {
            match (field, wire) {
                $( ($n, decode_fields!(@pat $kind v)) => decode_fields!(@set $out, $kind, $field, v), )*
                _ => {}
            }
        }
    }};
    (@pat str $v:ident) => { Wire::Len($v) };
    (@pat msg $v:ident) => { Wire::Len($v) };
    (@pat rep $v:ident) => { Wire::Len($v) };
    (@pat u64 $v:ident) => { Wire::Varint($v) };
    (@pat i64 $v:ident) => { Wire::Varint($v) };
    (@pat i32 $v:ident) => { Wire::Varint($v) };
    (@pat bool $v:ident) => { Wire::Varint($v) };
    (@set $o:ident, str, $f:ident, $v:ident) => { $o.$f = string($v) };
    (@set $o:ident, msg, $f:ident, $v:ident) => { $o.$f = Some(Decode::decode($v)?) };
    (@set $o:ident, rep, $f:ident, $v:ident) => { $o.$f.push(Decode::decode($v)?) };
    (@set $o:ident, u64, $f:ident, $v:ident) => { $o.$f = $v };
    (@set $o:ident, i64, $f:ident, $v:ident) => { $o.$f = $v as i64 };
    (@set $o:ident, i32, $f:ident, $v:ident) => { $o.$f = $v as i32 };
    (@set $o:ident, bool, $f:ident, $v:ident) => { $o.$f = $v != 0 };
}

/// A protobuf message this module can decode.
pub trait Decode: Sized {
    /// Decodes one message.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Format`] for malformed input.
    fn decode(buf: &[u8]) -> Result<Self, Sync2Error>;
}

macro_rules! record {
    ($(#[$doc:meta])* $name:ident { $($(#[$fdoc:meta])* $n:literal => $kind:ident $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct $name { $($(#[$fdoc])* pub $field: $ty,)* }
        impl Decode for $name {
            fn decode(buf: &[u8]) -> Result<Self, Sync2Error> {
                let mut out = Self::default();
                decode_fields!(buf, out, { $($n => $kind $field),* });
                Ok(out)
            }
        }
    };
}

record!(
    /// `Sync2.Message.SyncChunk`: one decrypted frame of the `message` database.
    MessageChunk {
        /// `PlatformType` of the sender: 1 mobile, 2 web, 3 PC.
        1 => i32 platform: i32,
        /// One entry per conversation.
        2 => rep partitions: Vec<Partition>,
    }
);

record!(
    /// `Sync2.Message.Partition`: messages of one conversation.
    Partition {
        /// `"oneone/<globalId>"` or `"group/<globalId>"`.
        1 => str id: String,
        /// Messages.
        2 => rep messages: Vec<Message>,
    }
);

record!(
    /// `Sync2.Message.Message`.
    Message {
        /// Global id of the sender.
        1 => str sender_id: String,
        /// `cliMsgId`.
        2 => u64 client_id: u64,
        /// Delivery status enum.
        3 => i32 msg_status: i32,
        /// Mobile message type (see [`crate::message::pc_msg_type`]).
        4 => i32 msg_type: i32,
        /// Unix milliseconds.
        5 => u64 timestamp: u64,
        /// `msgId` / `global_msg_id`.
        6 => i64 global_id: i64,
        /// Text content.
        7 => str content: String,
        /// Attachments, mentions, quote…
        8 => msg meta: Option<MetaData>,
    }
);

record!(
    /// `Sync2.Message.MetaData`.
    MetaData {
        /// Attachments.
        1 => rep attachs: Vec<Attach>,
        /// Mentions.
        2 => rep mentions: Vec<Mention>,
        /// Reference.
        3 => msg reference: Option<Reference>,
        /// Quoted message.
        4 => msg quote: Option<Quote>,
        /// Text properties.
        5 => msg property: Option<Properties>,
        /// Self-destruct time.
        6 => u64 ttl: u64,
    }
);

record!(
    /// `Sync2.Message.Attach`.
    Attach {
        /// `type`.
        1 => str kind: String,
        /// `extInfo`.
        2 => str ext_info: String,
        /// `action`, for example `rtf` or `recommened.user`.
        3 => str action: String,
        /// `params` (JSON text).
        4 => str params: String,
        /// `title`.
        5 => str title: String,
        /// `href`.
        6 => str href: String,
        /// `thumb`.
        7 => str thumb: String,
        /// `description`.
        8 => str description: String,
        /// `catId`.
        9 => i32 cat_id: i32,
        /// `id`.
        10 => i32 id: i32,
        /// `childNumber`.
        11 => i32 child_number: i32,
        /// `remains`.
        12 => str remains: String,
        /// `zinstantData`.
        13 => str zinstant_data: String,
        /// `zinstantMsg`.
        14 => str zinstant_msg: String,
    }
);

record!(
    /// `Sync2.Message.Mention`.
    Mention {
        /// Global id of the mentioned user, or `-1` for everyone.
        1 => str uid: String,
        /// Start offset.
        2 => i32 pos: i32,
        /// Length.
        3 => i32 len: i32,
        /// Kind.
        4 => i32 kind: i32,
    }
);

record!(
    /// `Sync2.Message.Quote`.
    Quote {
        /// `attach`.
        1 => str attach: String,
        /// Global id of the quoted message's sender.
        2 => str owner_id: String,
        /// `ts`.
        3 => i64 ts: i64,
        /// `fromD`.
        4 => str from_d: String,
        /// `cliMsgId`.
        5 => u64 cli_msg_id: u64,
        /// `globalMsgId`.
        6 => u64 global_msg_id: u64,
        /// `msg`.
        7 => str msg: String,
        /// `cliMsgType`.
        8 => i32 cli_msg_type: i32,
        /// `ttl`.
        9 => u64 ttl: u64,
        /// `partialQuote`.
        10 => str partial_quote: String,
    }
);

record!(
    /// `Sync2.Message.Properties`.
    Properties {
        /// `size`.
        1 => i32 size: i32,
        /// `color`.
        2 => i32 color: i32,
        /// `type`.
        3 => i32 kind: i32,
        /// `subType`.
        4 => i32 sub_type: i32,
        /// `ext`.
        5 => str ext: String,
    }
);

record!(
    /// `Sync2.Message.Reference`.
    Reference {
        /// `type`.
        1 => i32 kind: i32,
        /// `data`.
        2 => str data: String,
    }
);

record!(
    /// `Sync2.Conversation.SyncChunk`: one decrypted frame of the `conversation` database.
    ConversationChunk {
        /// `PlatformType`.
        1 => i32 platform: i32,
        /// Conversations.
        2 => rep conversations: Vec<Conversation>,
    }
);

record!(
    /// `Sync2.Conversation.Conversation`.
    Conversation {
        /// Global id of the conversation.
        1 => str conv_id: String,
        /// 1 one-to-one, 2 group.
        2 => i32 conv_type: i32,
        /// Whether this account replied in it.
        3 => bool responded_by_me: bool,
        /// Time of the last message.
        4 => u64 last_ts: u64,
        /// `globalId` of the last message.
        5 => u64 last_global_id: u64,
        /// `cliMsgId` of the last message.
        6 => u64 last_client_id: u64,
    }
);
