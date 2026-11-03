#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid store configuration: {0}")]
    Config(String),

    /// The writer queue is full. Returned only by the non-blocking
    /// [`Store::try_write`](crate::Store::try_write); the row was not queued.
    #[error("store queue is full; row dropped")]
    QueueFull,

    /// The writer thread has stopped.
    #[error("store writer has stopped")]
    Closed,

    #[error("QuestDB: {0}")]
    QuestDb(#[from] questdb::Error),

    #[error("query returned an unexpected shape: {0}")]
    Shape(String),
}
