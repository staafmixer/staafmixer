pub mod consts;
mod ingest;

use crate::ingest::ingest_server;
use tokio::task::spawn;

#[tokio::main]
async fn main() {
    let task = spawn(ingest_server());
    task.await.unwrap();
}
