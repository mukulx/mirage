#[tokio::main]
async fn main() -> anyhow::Result<()> {
    mirage_launcher::run().await
}
