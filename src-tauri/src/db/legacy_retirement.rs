//! Durable cutover of legacy automation; retained nodes and history are never
//! converted into Circuits or automatically restarted.

use rusqlite::{Connection, params};

pub(crate) fn retire() -> super::SqlResult<Vec<i64>> {
    let mut db = super::write_conn();
    retire_locked(&mut db)
}

fn retire_locked(db: &mut Connection) -> super::SqlResult<Vec<i64>> {
    let tx = db.transaction()?;
    tx.execute("INSERT OR IGNORE INTO legacy_autopilot_retirements(node_id,prior_state,session_started_at)
        SELECT r.node_id,r.state,a.session_started_at FROM autopilot_runs r JOIN agent_nodes a ON a.id=r.node_id
        WHERE r.state IN ('implementing','finishing','suffix_pending')", [])?;
    tx.execute("UPDATE autopilot_runs SET state='cancelled',updated_at=datetime('now')
        WHERE state IN ('implementing','finishing','suffix_pending')", [])?;
    tx.execute("UPDATE meshes SET autopilot_enabled=0 WHERE autopilot_enabled<>0", [])?;
    let pending = {
        let mut statement = tx.prepare("SELECT node_id FROM legacy_autopilot_retirements WHERE stopped_at IS NULL ORDER BY node_id")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<super::SqlResult<Vec<i64>>>()?
    };
    tx.commit()?;
    Ok(pending)
}

pub(crate) fn pending_inner(db: &Connection, node_id: i64) -> super::SqlResult<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM legacy_autopilot_retirements WHERE node_id=?1 AND stopped_at IS NULL)", [node_id], |row| row.get(0))
}

pub(crate) fn pending(node_id: i64) -> super::SqlResult<bool> {
    pending_inner(&super::read_conn(), node_id)
}

pub(crate) fn owns_stop(node_id: i64) -> super::SqlResult<bool> {
    owns_stop_inner(&super::read_conn(), node_id)
}

fn owns_stop_inner(db: &Connection, node_id: i64) -> super::SqlResult<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM legacy_autopilot_retirements r JOIN agent_nodes a ON a.id=r.node_id
        WHERE r.node_id=?1 AND r.stopped_at IS NULL AND r.session_started_at IS a.session_started_at
        AND NOT EXISTS(SELECT 1 FROM autopilot_circuit_runs c WHERE c.source_agent_node_id=a.id AND c.state IN ('pending','running','paused'))
        AND NOT EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s JOIN autopilot_circuit_runs c ON c.id=s.run_id
            WHERE s.agent_node_id=a.id AND c.state IN ('pending','running','paused')))", [node_id], |row| row.get(0))
}

pub(crate) fn acknowledge_stop(node_id: i64) -> super::SqlResult<()> {
    super::write_conn().execute("UPDATE legacy_autopilot_retirements SET stopped_at=datetime('now') WHERE node_id=?1", params![node_id])?;
    Ok(())
}

/// Keep retired nodes suspended: Idle is the frontend's fresh auto-spawn
/// signal. Commit this state with cleanup acknowledgement, rechecking ownership.
pub(crate) fn complete_stop(node_id: i64) -> super::SqlResult<()> {
    complete_stop_locked(&mut super::write_conn(), node_id)
}

fn complete_stop_locked(db: &mut Connection, node_id: i64) -> super::SqlResult<()> {
    let tx = db.transaction()?;
    let archived: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_nodes WHERE id=?1 AND status='archived')", [node_id], |row| row.get(0))?;
    if owns_stop_inner(&tx, node_id)? && !archived {
        super::agent_node::update_agent_node_status_inner(&tx, node_id, crate::models::SessionStatus::Suspended)?;
    }
    tx.execute("UPDATE legacy_autopilot_retirements SET stopped_at=datetime('now') WHERE node_id=?1", [node_id])?;
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_retirement_cleanup_preserves_archived_nodes() {
        let mut db = Connection::open_in_memory().unwrap();
        super::super::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path) VALUES(1,'test','/retained');
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(1,1,'retained','/retained','running');
            INSERT INTO autopilot_runs(node_id,mesh_id,issue_number,state) VALUES(1,1,18,'finishing');").unwrap();
        assert_eq!(retire_locked(&mut db).unwrap(), vec![1]);
        db.execute("UPDATE agent_nodes SET status='archived' WHERE id=1", []).unwrap();
        complete_stop_locked(&mut db, 1).unwrap();
        assert_eq!(db.query_row("SELECT status FROM agent_nodes WHERE id=1", [], |row| row.get::<_, String>(0)).unwrap(), "archived");
        assert!(!pending_inner(&db, 1).unwrap());
    }

    #[test]
    fn circuit_cutover_cancels_legacy_automation_without_transferring_or_deleting_work() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut db = Connection::open(file.path()).unwrap();
        super::super::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes(id,name,path,autopilot_enabled,autopilot_concurrency_limit,circuit_run_capacity)
            VALUES(1,'test','/retained',1,7,3);
            INSERT INTO agent_nodes(id,mesh_id,name,path,status) VALUES(1,1,'active','/retained/work','suspended'),(2,1,'done','/retained/done','completed');
            INSERT INTO autopilot_runs(node_id,mesh_id,issue_number,state,pr_url) VALUES(1,1,18,'finishing','https://example.test/pr/18'),(2,1,19,'completed','https://example.test/pr/19');").unwrap();
        assert_eq!(retire_locked(&mut db).unwrap(), vec![1]);
        drop(db);
        let mut db = Connection::open(file.path()).unwrap();
        assert_eq!(retire_locked(&mut db).unwrap(), vec![1], "stop remains pending after interrupted cleanup");
        assert!(pending_inner(&db,1).unwrap());
        assert!(owns_stop_inner(&db,1).unwrap());
        assert!(super::super::circuit::leases::claim_agent_spawn_inner(&db,1).unwrap().is_none(), "pending retirement excludes a replacement spawn");
        db.execute("UPDATE agent_nodes SET session_started_at=42 WHERE id=1",[]).unwrap();
        assert!(!owns_stop_inner(&db,1).unwrap(), "a new incarnation cannot be stopped by an old retirement");
        db.execute("UPDATE agent_nodes SET session_started_at=NULL WHERE id=1",[]).unwrap();
        db.execute_batch("INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
            INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state,source_agent_node_id) VALUES(1,1,1,'running',1);").unwrap();
        assert!(!owns_stop_inner(&db,1).unwrap(), "a newer Circuit borrower owns the retained process");
        db.execute("DELETE FROM autopilot_circuit_runs WHERE id=1",[]).unwrap();
        let row: (String,String) = db.query_row("SELECT state,pr_url FROM autopilot_runs WHERE node_id=1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(row,("cancelled".into(),"https://example.test/pr/18".into()));
        assert_eq!(db.query_row("SELECT state FROM autopilot_runs WHERE node_id=2",[],|r|r.get::<_,String>(0)).unwrap(),"completed");
        let settings: (bool,i32,i32) = db.query_row("SELECT autopilot_enabled,autopilot_concurrency_limit,circuit_run_capacity FROM meshes",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(settings,(false,7,3));
        assert_eq!(db.query_row("SELECT COUNT(*) FROM agent_nodes",[],|r|r.get::<_,i64>(0)).unwrap(),2);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM autopilot_circuit_runs",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert!(super::super::agent_node::list_suspended_nodes_inner(&db).unwrap().is_empty(), "retired work never auto-resumes");
        db.execute("UPDATE agent_nodes SET status='running' WHERE id=1",[]).unwrap();
        db.execute_batch("CREATE TRIGGER fail_retirement_ack BEFORE UPDATE OF stopped_at ON legacy_autopilot_retirements BEGIN SELECT RAISE(ABORT,'injected acknowledgement failure'); END;").unwrap();
        assert!(complete_stop_locked(&mut db,1).is_err());
        assert_eq!(db.query_row("SELECT status FROM agent_nodes WHERE id=1",[],|row|row.get::<_,String>(0)).unwrap(),"running","failed acknowledgement rolls back suspension");
        assert!(pending_inner(&db,1).unwrap());
        db.execute("DROP TRIGGER fail_retirement_ack",[]).unwrap();
        complete_stop_locked(&mut db,1).unwrap();
        assert_eq!(db.query_row("SELECT status FROM agent_nodes WHERE id=1",[],|row|row.get::<_,String>(0)).unwrap(),"suspended","Idle would automatically spawn in the terminal component");
        assert!(super::super::agent_node::list_suspended_nodes_inner(&db).unwrap().is_empty());
        assert!(retire_locked(&mut db).unwrap().is_empty());
    }
}
