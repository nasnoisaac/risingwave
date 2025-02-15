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

mod prometheus_gen;
pub use prometheus_gen::*;
mod frontend_gen;
pub use frontend_gen::*;
mod grafana_gen;
pub use grafana_gen::*;
mod zookeeper_gen;
pub use zookeeper_gen::*;
mod kafka_gen;
pub use kafka_gen::*;
