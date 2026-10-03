//! GraphQL execution with framework policy guards (feature `graphql`), on
//! top of `async-graphql`.

use async_graphql::{ObjectType, Request, Response, Schema, ServerError, SubscriptionType};

/// Runtime policy for framework-level GraphQL execution.
#[derive(Debug, Clone)]
pub struct GraphqlExecutionPolicy {
    /// Maximum accepted GraphQL query length in bytes.
    pub max_query_bytes: usize,
    /// Whether introspection queries are accepted.
    pub allow_introspection: bool,
}

impl Default for GraphqlExecutionPolicy {
    fn default() -> Self {
        Self {
            max_query_bytes: 16 * 1024,
            allow_introspection: true,
        }
    }
}

impl GraphqlExecutionPolicy {
    /// The same 16 KiB query limit as `default()`, with introspection
    /// disabled.
    pub fn production_default() -> Self {
        Self {
            max_query_bytes: 16 * 1024,
            allow_introspection: false,
        }
    }
}

/// The conventional path to mount a GraphQL endpoint at: `/graphql`.
pub fn default_graphql_path() -> &'static str {
    "/graphql"
}

/// Execute a GraphQL request with lightweight, framework-level policy guards.
pub async fn execute_graphql<Q, M, S>(
    schema: &Schema<Q, M, S>,
    request: Request,
    policy: &GraphqlExecutionPolicy,
) -> Response
where
    Q: ObjectType + Send + Sync + 'static,
    M: ObjectType + Send + Sync + 'static,
    S: SubscriptionType + Send + Sync + 'static,
{
    let query = request.query.trim();

    if query.len() > policy.max_query_bytes {
        return Response::from_errors(vec![ServerError::new(
            "graphql query exceeds max_query_bytes policy",
            None,
        )]);
    }

    if !policy.allow_introspection {
        if contains_introspection(query) {
            return Response::from_errors(vec![ServerError::new(
                "graphql introspection is disabled",
                None,
            )]);
        }
        // Defence in depth: the engine's own switch as well, in case a query
        // reaches an introspection field by a route the walk below misses.
        return schema.execute(request.disable_introspection()).await;
    }

    schema.execute(request).await
}

/// Whether `query` selects an introspection root field (`__schema`, `__type`).
///
/// Parses the document and walks every operation and fragment. The previous
/// check was a case-insensitive substring match, which refused ordinary
/// queries that merely mentioned `__type` in a string argument or a comment,
/// and never looked at field *names* specifically. `__typename` is not
/// introspection of the schema and stays allowed. A query that does not parse
/// is left to the engine, which rejects it with a proper parse error.
fn contains_introspection(query: &str) -> bool {
    use async_graphql::parser::types::{DocumentOperations, Selection, SelectionSet};

    fn walk(set: &SelectionSet) -> bool {
        set.items.iter().any(|item| match &item.node {
            Selection::Field(field) => {
                matches!(field.node.name.node.as_str(), "__schema" | "__type")
                    || walk(&field.node.selection_set.node)
            }
            Selection::InlineFragment(fragment) => walk(&fragment.node.selection_set.node),
            Selection::FragmentSpread(_) => false,
        })
    }

    let Ok(document) = async_graphql::parser::parse_query(query) else {
        return false;
    };
    let in_operations = match &document.operations {
        DocumentOperations::Single(op) => walk(&op.node.selection_set.node),
        DocumentOperations::Multiple(ops) => {
            ops.values().any(|op| walk(&op.node.selection_set.node))
        }
    };
    in_operations
        || document
            .fragments
            .values()
            .any(|fragment| walk(&fragment.node.selection_set.node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn introspection_is_detected_by_field_name_not_substring() {
        assert!(contains_introspection("{ __schema { types { name } } }"));
        assert!(contains_introspection(
            "{ t: __type(name: \"Query\") { name } }"
        ));
        assert!(contains_introspection(
            "query { ...F } fragment F on Query { __schema { queryType { name } } }"
        ));
        assert!(contains_introspection(
            "{ ... on Query { __type(name: \"X\") { name } } }"
        ));

        // Mentions that are not introspection fields.
        assert!(!contains_introspection("{ ping }"));
        assert!(!contains_introspection("{ __typename }"));
        assert!(!contains_introspection("{ search(q: \"__schema\") }"));
        assert!(!contains_introspection("# __type in a comment\n{ ping }"));
    }
    use async_graphql::{EmptyMutation, EmptySubscription, Object};

    struct QueryRoot;

    #[Object]
    impl QueryRoot {
        async fn ping(&self) -> &str {
            "pong"
        }
    }

    fn schema() -> Schema<QueryRoot, EmptyMutation, EmptySubscription> {
        Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish()
    }

    #[tokio::test]
    async fn executes_query_with_default_policy() {
        let response = execute_graphql(
            &schema(),
            Request::new("{ ping }"),
            &GraphqlExecutionPolicy::default(),
        )
        .await;

        assert!(response.errors.is_empty());
        assert_eq!(response.data.to_string(), r#"{ping: "pong"}"#);
    }

    #[tokio::test]
    async fn blocks_query_larger_than_policy_limit() {
        let policy = GraphqlExecutionPolicy {
            max_query_bytes: 3,
            allow_introspection: true,
        };

        let response = execute_graphql(&schema(), Request::new("{ ping }"), &policy).await;
        assert_eq!(response.errors.len(), 1);
        assert!(response.errors[0].message.contains("max_query_bytes"));
    }

    #[tokio::test]
    async fn blocks_introspection_when_disabled() {
        let policy = GraphqlExecutionPolicy {
            max_query_bytes: 1024,
            allow_introspection: false,
        };

        let response = execute_graphql(
            &schema(),
            Request::new("{ __schema { queryType { name } } }"),
            &policy,
        )
        .await;
        assert_eq!(response.errors.len(), 1);
        assert!(response.errors[0].message.contains("introspection"));
    }
}
