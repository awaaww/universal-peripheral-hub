use std::sync::Arc;

use hub_proto::{
    codes, method, ReadParams, Request, Response, RpcError, SetEnabledParams, WriteParams,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::service::Service;

/// Turn one request into at most one response. Notifications produce `None`.
///
/// Device work runs on a blocking worker: a DDC/CI write takes hundreds of
/// milliseconds and must never stall the pipe's io loop.
pub async fn dispatch(service: Arc<Service>, request: Request) -> Option<Response> {
    let id = request.id;
    let method_name = request.method.clone();
    let is_notification = request.is_notification();
    let params = request.params;

    if is_notification {
        match method_name.as_str() {
            method::ACTIVATE => {
                service.activate();
            }
            other => tracing::debug!(method = other, "ignoring unknown notification"),
        }
        return None;
    }

    let response = match method_name.as_str() {
        method::HELLO => success(id, &service.hello()),
        method::DEVICES_LIST => success(id, &service.snapshot()),
        method::CAPABILITY_READ => match parse::<ReadParams>(params) {
            Ok(params) => match blocking(service, move |svc| {
                svc.read(&params.device, &params.capability)
            })
            .await
            {
                Ok(result) => success(id, &result),
                Err(err) => Response::failure(id, err),
            },
            Err(err) => Response::failure(id, err),
        },
        method::CAPABILITY_WRITE => match parse::<WriteParams>(params) {
            Ok(params) => match blocking(service, move |svc| {
                svc.write(&params.device, &params.capability, &params.value)
            })
            .await
            {
                Ok(result) => success(id, &result),
                Err(err) => Response::failure(id, err),
            },
            Err(err) => Response::failure(id, err),
        },
        method::ADAPTER_SET_ENABLED => match parse::<SetEnabledParams>(params) {
            Ok(params) => match blocking(service, move |svc| {
                svc.set_enabled(&params.adapter, params.enabled)
            })
            .await
            {
                Ok(result) => success(id, &result),
                Err(err) => Response::failure(id, err),
            },
            Err(err) => Response::failure(id, err),
        },
        other => Response::failure(id, RpcError::method_not_found(other)),
    };
    tracing::debug!(
        method = method_name.as_str(),
        failed = response.error.is_some(),
        "rpc handled"
    );
    Some(response)
}

fn success<T: serde::Serialize>(id: Option<u64>, value: &T) -> Response {
    match serde_json::to_value(value) {
        Ok(value) => Response::success(id, value),
        Err(err) => Response::failure(
            id,
            RpcError::new(
                codes::INTERNAL_ERROR,
                format!("cannot serialise response: {err}"),
                None,
            ),
        ),
    }
}

fn parse<T: DeserializeOwned>(params: Value) -> std::result::Result<T, RpcError> {
    serde_json::from_value(params).map_err(|err| RpcError::invalid_params(err.to_string()))
}

async fn blocking<T, F>(service: Arc<Service>, work: F) -> std::result::Result<T, RpcError>
where
    T: Send + 'static,
    F: FnOnce(&Service) -> hub_core::Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || work(&service)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(err.into()),
        Err(join) => Err(RpcError::new(
            codes::INTERNAL_ERROR,
            format!("worker task failed: {join}"),
            None,
        )),
    }
}
