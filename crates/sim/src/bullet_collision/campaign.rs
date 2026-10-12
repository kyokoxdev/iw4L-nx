//! Script animation trees and attached-model tags on the authority DObj, for
//! campaign actors and their guns. Fork-only; kept out of the parent file so
//! upstream merges touch it as little as possible.

use super::AuthorityDObjState;

impl AuthorityDObjState {
    /// A script animation tree's nodes this tick (see `script_anim`).
    pub fn set_script_tree(&mut self, tree: Option<xmodel_runtime::XAnimTreeSnapshot>) {
        if self.play_anim.is_none() && self.semantic_state.tree == tree {
            return;
        }
        self.play_anim = None;
        self.pose_revision = self.pose_revision.wrapping_add(1);
        self.semantic_state.tree = tree;
        self.semantic_state.pose_revision = self.pose_revision;
        self.current_collision = None;
        self.materialized_pose_revision = None;
    }

    /// The base model with its attachments, as the client composes it.
    pub(super) fn composed(&self) -> Option<xmodel_runtime::DObj> {
        let capability = self.capability.as_ref()?;
        let mut models = vec![(&capability.pose, None)];
        models.extend(self.attached.iter().map(|(attached, tag)| {
            (
                &attached.pose,
                Some(xmodel_runtime::Attach {
                    parent_model: 0,
                    tag: tag.clone(),
                }),
            )
        }));
        xmodel_runtime::DObj::build(&models).ok()
    }
}

#[cfg(test)]
mod attached_tag_tests {
    use super::super::*;
    use std::sync::Arc;

    /// A root `tag_origin` and one child bone `child` at `offset`.
    fn model(
        name: &str,
        child: &str,
        offset: [f32; 3],
    ) -> Arc<xmodel_runtime::RetainedModelCapability> {
        Arc::new(xmodel_runtime::RetainedModelCapability {
            key: name.into(),
            pose: xmodel_runtime::ModelPoseSrc {
                name: name.into(),
                num_bones: 2,
                num_root_bones: 1,
                scale: 1.0,
                no_scale_part_bits: [0; 6],
                bone_names: vec!["tag_origin".into(), child.into()],
                parent_list: vec![1],
                quats: vec![[0, 0, 0, 32767]],
                trans: vec![offset],
                base_mat: vec![(glam::Quat::IDENTITY, glam::Vec3::ZERO); 2],
                root_rest: None,
            },
            bone_collision: Vec::new(),
            contents: None,
            coll_lod: 0,
            coll_surfs: Vec::new(),
            movement_brushes: Vec::new(),
            bounds: None,
            radius: None,
        })
    }

    #[test]
    fn attached_model_tags_answer_after_the_base_bones() {
        let body = model("body", "tag_weapon_right", [10.0, 0.0, 50.0]);
        let gun = model("gun", "tag_flash", [30.0, 0.0, 0.0]);
        let mut dobj = AuthorityDObjState::at_pose("body", Some(body), [100.0, 0.0, 0.0], [0.0; 3]);
        assert_eq!(dobj.tag_bone("tag_flash"), None);
        dobj.set_attachments(&[("gun", "tag_weapon_right", Some(gun.clone()))]);
        assert_eq!(dobj.tag_bone("tag_weapon_right"), Some(1));
        assert_eq!(dobj.tag_bone("tag_flash"), Some(3));
        let at = dobj
            .tag_world_matrix("tag_flash")
            .unwrap()
            .w_axis
            .truncate();
        assert!(
            (at - glam::Vec3::new(140.0, 0.0, 50.0)).length() < 1e-3,
            "{at}"
        );
        // One attachment without a capability would shift the client's bones.
        dobj.set_attachments(&[
            ("hat", "tag_origin", None),
            ("gun", "tag_weapon_right", Some(gun)),
        ]);
        assert_eq!(dobj.tag_bone("tag_flash"), None);
    }
}
