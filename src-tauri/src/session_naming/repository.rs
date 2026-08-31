use crate::models::AgentNode;

// ---------------------------------------------------------------------------
// Repository trait — abstracts DB calls for testability
// ---------------------------------------------------------------------------

pub(crate) trait SessionNamingRepository: Send + Sync {
    fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String>;
    fn update_agent_node_name(&self, id: i64, name: &str) -> Result<(), String>;
}

pub(crate) struct DbSessionNamingRepository;

impl SessionNamingRepository for DbSessionNamingRepository {
    fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String> {
        crate::db::get_agent_node_by_id(id).map_err(|e| e.to_string())
    }
    fn update_agent_node_name(&self, id: i64, name: &str) -> Result<(), String> {
        crate::db::update_agent_node_name(id, name).map_err(|e| e.to_string())
    }
}
