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
    pub fn production_default() -> Self {
        Self {
            max_query_bytes: 16 * 1024,
            allow_introspection: false,
        }
    }
}

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

    if !policy.allow_introspection && contains_introspection(query) {
        return Response::from_errors(vec![ServerError::new(
            "graphql introspection is disabled",
            None,
        )]);
    }

    schema.execute(request).await
}

fn contains_introspection(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains("__schema") || lower.contains("__type")
}

#[cfg(test)]
mod tests {
    use super::*;
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
