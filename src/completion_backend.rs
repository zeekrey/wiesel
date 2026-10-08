use crate::{EventScope, ResultEvent, ResultPayload, auth::DeviceCredential, gateway};
use anyhow::Context as _;
use std::sync::{Arc, mpsc};

pub(super) struct CompletionRequest {
    pub credential: Arc<DeviceCredential>,
    pub model: String,
    pub messages: Vec<gateway::Message>,
    pub chat: bool,
    pub scope: EventScope,
}

// There is only one production route. The controlled implementation is compiled
// only for tests, so neither credentials nor the fixed gateway origin can be bypassed.
pub(super) enum CompletionBackend {
    Gateway,
    #[cfg(test)]
    Controlled(ControlledBackend),
}

impl CompletionBackend {
    pub fn start(&self, request: CompletionRequest, tx: mpsc::Sender<ResultEvent>) {
        match self {
            Self::Gateway => {
                std::thread::spawn(move || {
                    let result = request
                        .credential
                        .ensure_valid()
                        .map_err(anyhow::Error::from)
                        .and_then(|()| {
                            if request.chat {
                                gateway::complete_stream(
                                    request.credential.access_token(),
                                    &request.model,
                                    &request.messages,
                                    |delta| {
                                        tx.send(ResultEvent {
                                            scope: request.scope,
                                            result: ResultPayload::ChatDelta(delta.to_owned()),
                                        })
                                        .context("Chat window closed")
                                    },
                                )
                            } else {
                                gateway::complete(
                                    request.credential.access_token(),
                                    &request.model,
                                    &request.messages,
                                )
                            }
                        });
                    let _ = tx.send(ResultEvent {
                        scope: request.scope,
                        result: ResultPayload::Completion(result, request.chat),
                    });
                });
            }
            #[cfg(test)]
            Self::Controlled(backend) => backend.start(request, tx),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(super) struct ControlledBackend(std::rc::Rc<std::cell::RefCell<Vec<RecordedRequest>>>);

#[cfg(test)]
struct RecordedRequest {
    model: String,
    messages: Vec<gateway::Message>,
    chat: bool,
    scope: EventScope,
    tx: mpsc::Sender<ResultEvent>,
    finished: bool,
}

#[cfg(test)]
impl ControlledBackend {
    fn start(&self, request: CompletionRequest, tx: mpsc::Sender<ResultEvent>) {
        // Test credentials are ephemeral but still pass the normal validity checks.
        request.credential.ensure_valid().unwrap();
        self.0.borrow_mut().push(RecordedRequest {
            model: request.model,
            messages: request.messages,
            chat: request.chat,
            scope: request.scope,
            tx,
            finished: false,
        });
    }

    pub fn request_count(&self) -> usize {
        self.0.borrow().len()
    }

    pub fn request(&self, index: usize) -> (String, Vec<gateway::Message>, bool) {
        let requests = self.0.borrow();
        let request = &requests[index];
        (
            request.model.clone(),
            request.messages.clone(),
            request.chat,
        )
    }

    pub fn delta(&self, index: usize, delta: &str) {
        let requests = self.0.borrow();
        let request = &requests[index];
        assert!(request.chat && !request.finished);
        request
            .tx
            .send(ResultEvent {
                scope: request.scope,
                result: ResultPayload::ChatDelta(delta.to_owned()),
            })
            .unwrap();
    }

    pub fn finish(&self, index: usize, result: anyhow::Result<String>) {
        let mut requests = self.0.borrow_mut();
        let request = &mut requests[index];
        assert!(!request.finished);
        request.finished = true;
        request
            .tx
            .send(ResultEvent {
                scope: request.scope,
                result: ResultPayload::Completion(result, request.chat),
            })
            .unwrap();
    }
}
