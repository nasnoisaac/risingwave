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

use std::sync::Arc;

use futures::Stream;
use risingwave_common::array::DataChunk;
use risingwave_common::error::Result;

use crate::session::SessionImpl;

mod distributed;
pub use distributed::QueryManager;
mod hummock_snapshot_manager;
pub use hummock_snapshot_manager::*;
mod plan_fragmenter;
pub use plan_fragmenter::BatchPlanFragmenter;
mod local;
pub use local::*;
mod task_context;
pub mod worker_node_manager;

pub trait DataChunkStream = Stream<Item = Result<DataChunk>>;

/// Context for mpp query execution.
pub struct ExecutionContext {
    session: Arc<SessionImpl>,
}

pub type ExecutionContextRef = Arc<ExecutionContext>;

impl ExecutionContext {
    pub fn new(session: Arc<SessionImpl>) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &SessionImpl {
        &self.session
    }
}
