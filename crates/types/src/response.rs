use {
    crate::{to_json_value, Attribute, Json, Message, StdResult},
    serde::{Deserialize, Serialize},
};

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub submsgs: Vec<SubMessage>,
    pub attributes: Vec<Attribute>,
}

impl Response {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_message(mut self, msg: Message) -> Self {
        self.submsgs.push(SubMessage::reply_never(msg));
        self
    }

    pub fn add_messages<M>(mut self, msgs: M) -> Self
    where
        M: IntoIterator<Item = Message>,
    {
        self.submsgs
            .extend(msgs.into_iter().map(SubMessage::reply_never));
        self
    }

    pub fn add_submessage(mut self, submsg: SubMessage) -> Self {
        self.submsgs.push(submsg);
        self
    }

    pub fn add_submessages<M>(mut self, submsgs: M) -> Self
    where
        M: IntoIterator<Item = SubMessage>,
    {
        self.submsgs.extend(submsgs);
        self
    }

    pub fn add_attribute<K, V>(mut self, key: K, value: V) -> Self
    where
        K: ToString,
        V: ToString,
    {
        self.attributes.push(Attribute::new(key, value));
        self
    }
}

/// Indicates that after a submessage has been executed, whether the host should
/// give the contract a callack.
///
/// The host's behavior is summariazed in the table below:
///
/// result | Success   | Error    | Always   | Never   |
/// ------ | --------- | -------- | -------- | ------- |
/// Ok     | callback  | nothing  | callback | nothing |
/// Err    | abort     | callback | callback | abort   |
///
/// In case a callback is to be performed, the host passes a piece of binary
/// payload data to the contract.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ReplyOn {
    Success(Json),
    Error(Json),
    Always(Json),
    Never,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SubMessage {
    pub msg: Message,
    pub reply_on: ReplyOn,
}

impl SubMessage {
    pub fn reply_never(msg: Message) -> Self {
        Self {
            msg,
            reply_on: ReplyOn::Never,
        }
    }

    pub fn reply_always<P>(msg: Message, payload: &P) -> StdResult<Self>
    where
        P: Serialize,
    {
        Ok(Self {
            msg,
            reply_on: ReplyOn::Always(to_json_value(payload)?),
        })
    }

    pub fn reply_on_success<P>(msg: Message, payload: &P) -> StdResult<Self>
    where
        P: Serialize,
    {
        Ok(Self {
            msg,
            reply_on: ReplyOn::Success(to_json_value(payload)?),
        })
    }

    pub fn reply_on_error<P>(msg: Message, payload: &P) -> StdResult<Self>
    where
        P: Serialize,
    {
        Ok(Self {
            msg,
            reply_on: ReplyOn::Error(to_json_value(payload)?),
        })
    }
}
