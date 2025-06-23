use crate::{
    connections::discovery::hole_punch::HolePunchError,
    shares::{EntryParseError, ScanDirError},
    ui_messages::UiServerError,
    wishlist::DbError,
    RequestError,
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An error in response to a UI command
#[derive(Serialize, Deserialize, PartialEq, Debug, Error, Clone)]
pub struct UiServerErrorWrapper(pub UiServerError);

impl std::fmt::Display for UiServerErrorWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<UiServerError> for UiServerErrorWrapper {
    fn from(error: UiServerError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(error)
    }
}

impl<T> From<std::sync::PoisonError<T>> for UiServerErrorWrapper {
    fn from(_error: std::sync::PoisonError<T>) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::Poison)
    }
}

impl From<HolePunchError> for UiServerErrorWrapper {
    fn from(error: HolePunchError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::PeerDiscovery(error.to_string()))
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for UiServerErrorWrapper {
    fn from(error: tokio::sync::oneshot::error::RecvError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::PeerDiscovery(error.to_string()))
    }
}

impl From<DbError> for UiServerErrorWrapper {
    fn from(error: DbError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::Db(error.to_string()))
    }
}

impl From<EntryParseError> for UiServerErrorWrapper {
    fn from(error: EntryParseError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::Db(error.to_string()))
    }
}

impl From<RequestError> for UiServerErrorWrapper {
    fn from(error: RequestError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::RequestError(error.to_string()))
    }
}

impl From<ScanDirError> for UiServerErrorWrapper {
    fn from(error: ScanDirError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::AddShare(error.to_string()))
    }
}

impl From<quinn::ConnectionError> for UiServerErrorWrapper {
    fn from(error: quinn::ConnectionError) -> UiServerErrorWrapper {
        UiServerErrorWrapper(UiServerError::ConnectionError(error.to_string()))
    }
}

impl IntoResponse for UiServerErrorWrapper {
    fn into_response(self) -> Response {
        log::error!("{self:?}");
        (StatusCode::INTERNAL_SERVER_ERROR, Json(self.0)).into_response()
    }
}
