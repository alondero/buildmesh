use crate::models::AgentNode;
use crate::services::transcript_reader::UnavailableReason;

// Naming dependencies — persistence and native dialogue acquisition.

pub(crate) trait SessionNamingRepository: Send + Sync {
    fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String>;
    fn commit_name(&self, input: &super::engine::RenameInput, name: &str) -> Result<bool, String>;
    fn naming_context(&self, node: &AgentNode) -> Result<String, UnavailableReason> {
        super::context::read(node)
    }
}

pub(crate) struct DbSessionNamingRepository;

impl SessionNamingRepository for DbSessionNamingRepository {
    fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String> {
        crate::db::get_agent_node_by_id(id).map_err(|e| e.to_string())
    }
    fn commit_name(&self, input: &super::engine::RenameInput, name: &str) -> Result<bool, String> {
        crate::db::auto_name_agent_node(
            input.node_id,
            &input.name,
            input.session_id.as_deref(),
            name,
        )
        .map_err(|e| e.to_string())
    }
}
