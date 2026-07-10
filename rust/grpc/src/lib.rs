//! The Theseus gRPC service surface: the generated wire glue, reusable by the
//! server binary and by tests that compose a server in-process.

pub mod generated;

#[cfg(test)]
mod tests {
    use theseus::{SnapshotRetention, TheseusService};

    use crate::generated::{
        GrpcTheseus,
        proto::{self, theseus_server::Theseus},
    };

    struct PruneEcho;

    #[async_trait::async_trait]
    impl TheseusService for PruneEcho {
        async fn prune(&self, request: SnapshotRetention) -> anyhow::Result<String> {
            Ok(format!("kept {}", request.keep))
        }
    }

    #[tokio::test]
    async fn prune_rejects_an_omitted_limit_but_accepts_explicit_zero() {
        let service = GrpcTheseus(PruneEcho);

        let error = service
            .prune(tonic::Request::new(proto::SnapshotRetention { keep: None }))
            .await
            .expect_err("an omitted required scalar is invalid");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("`keep`"), "{error}");

        let reply = service
            .prune(tonic::Request::new(proto::SnapshotRetention {
                keep: Some(0),
            }))
            .await
            .expect("an explicit zero is a valid retention limit")
            .into_inner();
        assert_eq!(reply.value, "kept 0");
    }
}
