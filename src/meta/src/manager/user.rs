// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;

use risingwave_common::catalog::{DEFAULT_SUPPER_USER, DEFAULT_SUPPER_USER_PASSWORD};
use risingwave_common::error::ErrorCode::InternalError;
use risingwave_common::error::{Result, RwError};
use risingwave_pb::user::auth_info::EncryptionType;
use risingwave_pb::user::grant_privilege::{PrivilegeWithGrantOption, Target};
use risingwave_pb::user::{AuthInfo, GrantPrivilege, UserInfo};
use tokio::sync::Mutex;

use crate::manager::MetaSrvEnv;
use crate::model::{MetadataModel, Transactional};
use crate::storage::{MetaStore, Transaction};

/// We use `UserName` as the key for the user info.
type UserName = String;

/// `UserManager` managers the user info, including authentication and privileges. It only responds
/// to manager the user info and some basic validation. Other authorization relate to the current
/// session user should be done in Frontend before passing to Meta.
pub struct UserManager<S: MetaStore> {
    env: MetaSrvEnv<S>,
    core: Mutex<HashMap<UserName, UserInfo>>,
}

impl<S: MetaStore> UserManager<S> {
    pub async fn new(env: MetaSrvEnv<S>) -> Result<Self> {
        let users = UserInfo::list(env.meta_store()).await?;
        let user_manager = Self {
            env,
            core: Mutex::new(HashMap::from_iter(
                users.into_iter().map(|user| (user.name.clone(), user)),
            )),
        };
        user_manager.init().await?;
        Ok(user_manager)
    }

    async fn init(&self) -> Result<()> {
        let mut core = self.core.lock().await;
        if !core.contains_key(DEFAULT_SUPPER_USER) {
            let default_user = UserInfo {
                name: DEFAULT_SUPPER_USER.to_string(),
                is_supper: true,
                can_create_db: true,
                can_login: true,
                auth_info: Some(AuthInfo {
                    encryption_type: EncryptionType::Plaintext as i32,
                    encrypted_value: Vec::from(DEFAULT_SUPPER_USER_PASSWORD.as_bytes()),
                }),
                ..Default::default()
            };

            default_user.insert(self.env.meta_store()).await?;
            core.insert(DEFAULT_SUPPER_USER.to_string(), default_user);
        }

        Ok(())
    }

    pub async fn list_users(&self) -> Result<Vec<UserInfo>> {
        let core = self.core.lock().await;
        Ok(core.values().cloned().collect())
    }

    pub async fn create_user(&self, user: &UserInfo) -> Result<()> {
        let mut core = self.core.lock().await;
        if core.contains_key(&user.name) {
            return Err(RwError::from(InternalError(format!(
                "User {} already exists",
                user.name
            ))));
        }
        user.insert(self.env.meta_store()).await?;
        core.insert(user.name.clone(), user.clone());
        // TODO: add notification support.
        Ok(())
    }

    pub async fn get_user(&self, user_name: &UserName) -> Result<UserInfo> {
        let core = self.core.lock().await;

        core.get(user_name)
            .cloned()
            .ok_or_else(|| RwError::from(InternalError(format!("User {} not found", user_name))))
    }

    pub async fn drop_user(&self, user_name: &UserName) -> Result<()> {
        let mut core = self.core.lock().await;
        if !core.contains_key(user_name) {
            return Err(RwError::from(InternalError(format!(
                "User {} does not exist",
                user_name
            ))));
        }
        if user_name == DEFAULT_SUPPER_USER {
            return Err(RwError::from(InternalError(format!(
                "Cannot drop default super user {}",
                user_name
            ))));
        }
        if !core.get(user_name).unwrap().grant_privileges.is_empty() {
            return Err(RwError::from(InternalError(format!(
                "Cannot drop user {} with privileges",
                user_name
            ))));
        }

        // TODO: add more check, like whether he owns any database/schema/table/source.
        UserInfo::delete(self.env.meta_store(), user_name).await?;
        core.remove(user_name);

        // TODO: add notification support.
        Ok(())
    }
}

/// Defines privilege grant for a user.
impl<S: MetaStore> UserManager<S> {
    // Merge new granted privilege.
    #[inline(always)]
    fn merge_privilege(origin_privilege: &mut GrantPrivilege, new_privilege: &GrantPrivilege) {
        assert_eq!(origin_privilege.target, new_privilege.target);

        let mut privilege_map = HashMap::<i32, bool>::from_iter(
            origin_privilege
                .privilege_with_opts
                .iter()
                .map(|po| (po.privilege, po.with_grant_option)),
        );
        for npo in &new_privilege.privilege_with_opts {
            if let Some(po) = privilege_map.get_mut(&npo.privilege) {
                *po |= npo.with_grant_option;
            } else {
                privilege_map.insert(npo.privilege, npo.with_grant_option);
            }
        }
        origin_privilege.privilege_with_opts = privilege_map
            .into_iter()
            .map(|(privilege, with_grant_option)| PrivilegeWithGrantOption {
                privilege,
                with_grant_option,
            })
            .collect();
    }

    pub async fn grant_privilege(
        &self,
        user_name: &UserName,
        new_grant_privileges: &[GrantPrivilege],
    ) -> Result<()> {
        let mut core = self.core.lock().await;
        let mut user = core
            .get(user_name)
            .ok_or_else(|| InternalError(format!("User {} does not exist", user_name)))
            .cloned()?;

        if user.is_supper {
            return Err(RwError::from(InternalError(format!(
                "Cannot grant privilege to supper user {}",
                user_name
            ))));
        }

        new_grant_privileges.iter().for_each(|new_grant_privilege| {
            if let Some(privilege) = user
                .grant_privileges
                .iter_mut()
                .find(|p| p.target == new_grant_privilege.target)
            {
                Self::merge_privilege(privilege, new_grant_privilege);
            } else {
                user.grant_privileges.push(new_grant_privilege.clone());
            }
        });

        user.insert(self.env.meta_store()).await?;
        core.insert(user_name.clone(), user);
        Ok(())
    }

    // Revoke privilege from target.
    #[inline(always)]
    fn revoke_privilege_inner(
        origin_privilege: &mut GrantPrivilege,
        revoke_grant_privilege: &GrantPrivilege,
        revoke_grant_option: bool,
    ) {
        assert_eq!(origin_privilege.target, revoke_grant_privilege.target);

        if revoke_grant_option {
            // Only revoke with grant option.
            origin_privilege
                .privilege_with_opts
                .iter_mut()
                .for_each(|po| {
                    if revoke_grant_privilege
                        .privilege_with_opts
                        .iter()
                        .any(|ro| ro.privilege == po.privilege)
                    {
                        po.with_grant_option = false;
                    }
                })
        } else {
            // Revoke all privileges matched with revoke_grant_privilege.
            origin_privilege.privilege_with_opts.retain(|po| {
                !revoke_grant_privilege
                    .privilege_with_opts
                    .iter()
                    .any(|ro| ro.privilege == po.privilege)
            });
        }
    }

    pub async fn revoke_privilege(
        &self,
        user_name: &UserName,
        revoke_grant_privileges: &[GrantPrivilege],
        revoke_grant_option: bool,
    ) -> Result<()> {
        let mut core = self.core.lock().await;
        let mut user = core
            .get(user_name)
            .ok_or_else(|| InternalError(format!("User {} does not exist", user_name)))
            .cloned()?;

        if user.is_supper {
            return Err(RwError::from(InternalError(format!(
                "Cannot revoke privilege from supper user {}",
                user_name
            ))));
        }

        let mut empty_privilege = false;
        revoke_grant_privileges
            .iter()
            .for_each(|revoke_grant_privilege| {
                for privilege in &mut user.grant_privileges {
                    if privilege.target == revoke_grant_privilege.target {
                        Self::revoke_privilege_inner(
                            privilege,
                            revoke_grant_privilege,
                            revoke_grant_option,
                        );
                        empty_privilege |= privilege.privilege_with_opts.is_empty();
                        break;
                    }
                }
            });

        if empty_privilege {
            user.grant_privileges
                .retain(|privilege| !privilege.privilege_with_opts.is_empty());
        }

        user.insert(self.env.meta_store()).await?;
        core.insert(user_name.clone(), user);
        Ok(())
    }

    /// `release_privileges` removes the privileges with given target from all users, it will be
    /// called when a database/schema/table/source is dropped.
    pub async fn release_privileges(&self, target: &Target) -> Result<()> {
        let mut core = self.core.lock().await;
        let mut transaction = Transaction::default();
        let mut users_need_update = vec![];
        for user in core.values() {
            let cnt = user.grant_privileges.len();
            let mut user = user.clone();
            user.grant_privileges
                .retain(|p| p.target.as_ref().unwrap() != target);
            if cnt != user.grant_privileges.len() {
                user.upsert_in_transaction(&mut transaction)?;
                users_need_update.push(user);
            }
        }
        self.env.meta_store().txn(transaction).await?;
        for user in users_need_update {
            core.insert(user.name.clone(), user);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use risingwave_pb::user::grant_privilege::{GrantTable, Privilege};

    use super::*;

    fn make_test_user(name: &str) -> UserInfo {
        UserInfo {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn make_privilege(
        target: Target,
        privileges: &[Privilege],
        with_grant_option: bool,
    ) -> GrantPrivilege {
        GrantPrivilege {
            target: Some(target),
            privilege_with_opts: privileges
                .iter()
                .map(|&p| PrivilegeWithGrantOption {
                    privilege: p as i32,
                    with_grant_option,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn test_user_manager() -> Result<()> {
        let user_manager = UserManager::new(MetaSrvEnv::for_test().await).await?;
        let test_user = "test_user";
        user_manager.create_user(&make_test_user(test_user)).await?;
        assert!(user_manager
            .create_user(&make_test_user(DEFAULT_SUPPER_USER))
            .await
            .is_err());

        let users = user_manager.list_users().await?;
        assert_eq!(users.len(), 2);

        let target = Target::GrantTable(GrantTable {
            database_id: 0,
            schema_id: 0,
            table_id: 0,
        });
        // Grant Select/Insert without grant option.
        user_manager
            .grant_privilege(
                &test_user.to_string(),
                &[make_privilege(
                    target.clone(),
                    &[Privilege::Select, Privilege::Insert],
                    false,
                )],
            )
            .await?;
        let user = user_manager.get_user(&test_user.to_string()).await?;
        assert_eq!(user.grant_privileges.len(), 1);
        assert_eq!(user.grant_privileges[0].target, Some(target.clone()));
        assert_eq!(user.grant_privileges[0].privilege_with_opts.len(), 2);
        assert!(user.grant_privileges[0]
            .privilege_with_opts
            .iter()
            .all(|p| !p.with_grant_option));

        // Grant Select/Insert with grant option.
        user_manager
            .grant_privilege(
                &test_user.to_string(),
                &[make_privilege(
                    target.clone(),
                    &[Privilege::Select, Privilege::Insert],
                    true,
                )],
            )
            .await?;
        let user = user_manager.get_user(&test_user.to_string()).await?;
        assert_eq!(user.grant_privileges.len(), 1);
        assert_eq!(user.grant_privileges[0].target, Some(target.clone()));
        assert_eq!(user.grant_privileges[0].privilege_with_opts.len(), 2);
        assert!(user.grant_privileges[0]
            .privilege_with_opts
            .iter()
            .all(|p| p.with_grant_option));

        // Grant Select/Update/Delete with grant option, while Select is duplicated.
        user_manager
            .grant_privilege(
                &test_user.to_string(),
                &[make_privilege(
                    target.clone(),
                    &[Privilege::Select, Privilege::Update, Privilege::Delete],
                    true,
                )],
            )
            .await?;
        let user = user_manager.get_user(&test_user.to_string()).await?;
        assert_eq!(user.grant_privileges.len(), 1);
        assert_eq!(user.grant_privileges[0].target, Some(target.clone()));
        assert_eq!(user.grant_privileges[0].privilege_with_opts.len(), 4);
        assert!(user.grant_privileges[0]
            .privilege_with_opts
            .iter()
            .all(|p| p.with_grant_option));

        // Revoke Select/Update/Delete/Insert with grant option.
        user_manager
            .revoke_privilege(
                &test_user.to_string(),
                &[make_privilege(
                    target.clone(),
                    &[
                        Privilege::Select,
                        Privilege::Insert,
                        Privilege::Delete,
                        Privilege::Update,
                    ],
                    false,
                )],
                true,
            )
            .await?;
        let user = user_manager.get_user(&test_user.to_string()).await?;
        assert_eq!(user.grant_privileges[0].privilege_with_opts.len(), 4);
        assert!(user.grant_privileges[0]
            .privilege_with_opts
            .iter()
            .all(|p| !p.with_grant_option));

        // Revoke Select/Delete/Insert.
        user_manager
            .revoke_privilege(
                &test_user.to_string(),
                &[make_privilege(
                    target.clone(),
                    &[Privilege::Select, Privilege::Insert, Privilege::Delete],
                    false,
                )],
                false,
            )
            .await?;
        let user = user_manager.get_user(&test_user.to_string()).await?;
        assert_eq!(user.grant_privileges.len(), 1);
        assert_eq!(user.grant_privileges[0].privilege_with_opts.len(), 1);

        // Release all privileges with target.
        user_manager.release_privileges(&target).await?;
        let user = user_manager.get_user(&test_user.to_string()).await?;
        assert!(user.grant_privileges.is_empty());

        Ok(())
    }
}
