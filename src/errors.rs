use reth_evm::execute::BlockExecutionError;

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum GnosisBlockExecutionError {
    #[error("Error: {message:?}")]
    CustomErrorMessage { message: String },
}

impl From<GnosisBlockExecutionError> for BlockExecutionError {
    fn from(err: GnosisBlockExecutionError) -> Self {
        Self::other(err)
    }
}
