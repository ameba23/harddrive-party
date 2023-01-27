pub mod messages {
    include!(concat!(env!("OUT_DIR"), "/harddriveparty.messages.rs"));
}

// pub mod fs;
// pub mod protocol;
pub mod rpc;
// pub mod run;
pub mod connect;
pub mod hdp;
pub mod shares;
