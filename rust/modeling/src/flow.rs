//! Flow extraction: which ports an authored handler reaches.
//!
//! An authored handler reaches a port through the composition root's field —
//! `self.workspace.write_file(..)` as a call receiver, or `self.workspace`
//! handed to a helper. This reads the authored impl and reports, per handler,
//! the set of port fields its body touches, so [`verify`](crate::verify) can
//! hold the modeled `uses` edges against the code.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::ImplItem;

/// Why flows could not be extracted.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// The authored impl source did not parse as Rust.
    #[error("parsing the authored impl: {0}")]
    Parse(String),
}

/// The ports each authored handler reaches, keyed by method name. Methods are
/// read from the `impl <trait_name> for …` block, and `ports` bounds the
/// `self` fields that count as a reach.
pub fn handler_flows(
    source: &str,
    trait_name: &str,
    ports: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeSet<String>>, FlowError> {
    let file = syn::parse_file(source).map_err(|error| FlowError::Parse(error.to_string()))?;
    let mut flows = BTreeMap::new();
    for block in crate::implement::trait_impls(&file, trait_name) {
        for impl_item in &block.items {
            if let ImplItem::Fn(method) = impl_item {
                let mut reached = BTreeSet::new();
                collect_reaches(method.block.to_token_stream(), ports, &mut reached);
                flows.insert(method.sig.ident.to_string(), reached);
            }
        }
    }
    Ok(flows)
}

/// Walk a token stream for `self . <port>` triples, descending into every
/// group. The scan reads tokens, so a reach stays visible in argument position
/// and inside macro bodies alike.
fn collect_reaches(tokens: TokenStream, ports: &BTreeSet<String>, reached: &mut BTreeSet<String>) {
    let mut after_self = false;
    let mut after_dot = false;
    for tree in tokens {
        match &tree {
            TokenTree::Group(group) => {
                collect_reaches(group.stream(), ports, reached);
                (after_self, after_dot) = (false, false);
            }
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                if after_self && after_dot && ports.contains(&name) {
                    reached.insert(name);
                    (after_self, after_dot) = (false, false);
                } else {
                    (after_self, after_dot) = (name == "self", false);
                }
            }
            TokenTree::Punct(punct) if punct.as_char() == '.' && after_self => {
                after_dot = true;
            }
            _ => (after_self, after_dot) = (false, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ports(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_reach_counts_as_receiver_and_as_argument() {
        let source = r#"
            impl SampleService for Ctx {
                async fn run(&self) -> anyhow::Result<()> {
                    self.toolchain.check().await?;
                    persist(&proposed, self.workspace).await
                }
            }
        "#;
        let flows =
            handler_flows(source, "SampleService", &ports(&["workspace", "toolchain"])).unwrap();
        assert_eq!(flows["run"], ports(&["workspace", "toolchain"]));
    }

    #[test]
    fn non_port_fields_and_literals_do_not_count() {
        let source = r#"
            impl SampleService for Ctx {
                async fn describe(&self) -> anyhow::Result<String> {
                    let text = "self.workspace stays a string";
                    Ok(format!("{} {text}", describe(self.model)))
                }
            }
        "#;
        let flows = handler_flows(source, "SampleService", &ports(&["workspace"])).unwrap();
        assert!(flows["describe"].is_empty());
    }

    #[test]
    fn a_reach_inside_a_macro_body_counts() {
        let source = r#"
            impl SampleService for Ctx {
                async fn run(&self) -> anyhow::Result<String> {
                    Ok(format!("{:?}", self.toolchain.check().await?))
                }
            }
        "#;
        let flows = handler_flows(source, "SampleService", &ports(&["toolchain"])).unwrap();
        assert_eq!(flows["run"], ports(&["toolchain"]));
    }

    #[test]
    fn methods_outside_the_trait_impl_are_ignored() {
        let source = r#"
            fn helper(w: &dyn Workspace) {}
            impl Other for Ctx {
                fn run(&self) { self.workspace.write(); }
            }
        "#;
        let flows = handler_flows(source, "SampleService", &ports(&["workspace"])).unwrap();
        assert!(flows.is_empty());
    }

    #[test]
    fn unparseable_source_is_an_error() {
        assert!(matches!(
            handler_flows("fn (", "SampleService", &ports(&[])),
            Err(FlowError::Parse(_))
        ));
    }
}
