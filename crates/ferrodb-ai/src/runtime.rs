//! The tokio runtime shared by the assistant (HTTP streaming) and the MCP
//! server. Created on first use; the UI thread never blocks on it.

use std::sync::OnceLock;

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ferrodb-ai")
            .enable_all()
            .build()
            .expect("start the ferrodb-ai runtime")
    })
}
