//! Form/explode list filters shared by the gateway, owners, and service clients.

use serde::{Serialize, de::DeserializeOwned};

use super::{AgentRunListQuery, HttpContractError, ResourceRequestListQuery};
use crate::CourseId;

struct StateListQuery<S> {
    state: Option<Vec<S>>,
    cursor: Option<String>,
    limit: Option<u16>,
    course_id: Option<CourseId>,
}

impl<S: DeserializeOwned> StateListQuery<S> {
    fn decode(query: &str, allow_course: bool) -> Result<Self, HttpContractError> {
        let mut result = Self {
            state: None,
            cursor: None,
            limit: None,
            course_id: None,
        };
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                // An empty array element contributes no filter, including `?state=`.
                "state" if value.is_empty() => {}
                "state" => {
                    let state = S::deserialize(serde::de::value::StrDeserializer::<
                        serde::de::value::Error,
                    >::new(&value))
                    .map_err(|_| HttpContractError::InvalidListQuery)?;
                    result.state.get_or_insert_with(Vec::new).push(state);
                }
                "cursor" if result.cursor.is_none() => result.cursor = Some(value.into_owned()),
                "limit" if result.limit.is_none() => {
                    result.limit = Some(
                        value
                            .parse()
                            .map_err(|_| HttpContractError::InvalidListQuery)?,
                    );
                }
                "courseId" if allow_course && result.course_id.is_none() => {
                    result.course_id = Some(
                        value
                            .parse()
                            .map_err(|_| HttpContractError::InvalidListQuery)?,
                    );
                }
                // Unknown fields and repeated scalars are never silently discarded.
                _ => return Err(HttpContractError::InvalidListQuery),
            }
        }
        Ok(result)
    }
}

fn encode<S: Serialize>(
    states: Option<&[S]>,
    cursor: Option<&str>,
    limit: Option<u16>,
    course_id: Option<CourseId>,
) -> Result<String, HttpContractError> {
    let mut query = form_urlencoded::Serializer::new(String::new());
    if let Some(course_id) = course_id {
        query.append_pair("courseId", &course_id.to_string());
    }
    for state in states.unwrap_or_default() {
        let value = serde_json::to_value(state).map_err(|_| HttpContractError::InvalidListQuery)?;
        query.append_pair(
            "state",
            value.as_str().ok_or(HttpContractError::InvalidListQuery)?,
        );
    }
    if let Some(cursor) = cursor {
        query.append_pair("cursor", cursor);
    }
    if let Some(limit) = limit {
        query.append_pair("limit", &limit.to_string());
    }
    Ok(query.finish())
}

impl AgentRunListQuery {
    /// Decodes the contract's form/explode query, rejecting unknown values and duplicate scalars.
    pub fn from_query(query: &str) -> Result<Self, HttpContractError> {
        let parsed = StateListQuery::decode(query, false)?;
        Ok(Self {
            state: parsed.state,
            cursor: parsed.cursor,
            limit: parsed.limit,
        })
    }

    /// Encodes each state under its own `state` key, as required by the public OpenAPI contract.
    pub fn to_query(&self) -> Result<String, HttpContractError> {
        encode(
            self.state.as_deref(),
            self.cursor.as_deref(),
            self.limit,
            None,
        )
    }
}

impl ResourceRequestListQuery {
    /// Decodes the contract's form/explode query, rejecting unknown values and duplicate scalars.
    pub fn from_query(query: &str) -> Result<Self, HttpContractError> {
        let parsed = StateListQuery::decode(query, true)?;
        Ok(Self {
            state: parsed.state,
            cursor: parsed.cursor,
            limit: parsed.limit,
            course_id: parsed.course_id,
        })
    }

    /// Encodes the same repeated-key representation consumed by the gateway and Resource owner.
    pub fn to_query(&self) -> Result<String, HttpContractError> {
        encode(
            self.state.as_deref(),
            self.cursor.as_deref(),
            self.limit,
            self.course_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{authoring::AgentRunState, resource::ResourceRequestState};

    #[test]
    fn exploded_states_round_trip_without_losing_scalars() -> Result<(), HttpContractError> {
        let agent =
            AgentRunListQuery::from_query("state=requested&state=running&limit=50&cursor=abc_-~")?;
        assert_eq!(
            agent.state,
            Some(vec![AgentRunState::Requested, AgentRunState::Running])
        );
        assert_eq!(
            agent.to_query()?,
            "state=requested&state=running&cursor=abc_-%7E&limit=50"
        );
        assert_eq!(AgentRunListQuery::from_query(&agent.to_query()?)?, agent);
        let resource = ResourceRequestListQuery::from_query(
            "courseId=01900000-0000-7000-8000-000000000001&state=reviewing&state=active&limit=50&cursor=abc",
        )?;
        assert_eq!(
            resource.state,
            Some(vec![
                ResourceRequestState::Reviewing,
                ResourceRequestState::Active
            ])
        );
        assert_eq!(
            ResourceRequestListQuery::from_query(&resource.to_query()?)?,
            resource
        );
        Ok(())
    }

    #[test]
    fn absent_empty_and_single_states_follow_the_contract() -> Result<(), HttpContractError> {
        for raw in ["", "state", "state=", "state=&state="] {
            assert_eq!(
                AgentRunListQuery::from_query(raw)?,
                AgentRunListQuery::default()
            );
            assert_eq!(
                ResourceRequestListQuery::from_query(raw)?,
                ResourceRequestListQuery::default()
            );
        }
        assert_eq!(
            AgentRunListQuery::from_query("state=requested")?.state,
            Some(vec![AgentRunState::Requested])
        );
        assert_eq!(
            ResourceRequestListQuery::from_query("state=reviewing")?.state,
            Some(vec![ResourceRequestState::Reviewing])
        );
        assert_eq!(
            AgentRunListQuery::from_query("state=&state=running")?.state,
            Some(vec![AgentRunState::Running])
        );
        assert_eq!(
            AgentRunListQuery::from_query("%73tate=%72unning")?.state,
            Some(vec![AgentRunState::Running])
        );
        Ok(())
    }

    #[test]
    fn unknown_values_and_duplicate_scalars_are_rejected() {
        for raw in [
            "state=unknown",
            "state=running&state=unknown",
            "state=running,requested",
            "state[]=running",
            "limit=50&limit=50",
            "cursor=a&cursor=b",
            "cursor=&cursor=b",
            "unknown=value",
            "limit=bad",
        ] {
            assert!(AgentRunListQuery::from_query(raw).is_err(), "{raw}");
        }
        for raw in [
            "state=unknown",
            "state=active&state=unknown",
            "state=active,reviewing",
            "state[]=active",
            "limit=50&limit=50",
            "cursor=a&cursor=b",
            "cursor=&cursor=b",
            "unknown=value",
            "courseId=01900000-0000-7000-8000-000000000001&courseId=01900000-0000-7000-8000-000000000001",
        ] {
            assert!(ResourceRequestListQuery::from_query(raw).is_err(), "{raw}");
        }
        assert!(
            AgentRunListQuery::from_query("courseId=01900000-0000-7000-8000-000000000001").is_err()
        );
    }
}
