//! MAM message syntax version distinguished between incompatible changes in Protobuf3
//! syntax and/or rules of processing Protobuf3 messages. It usually means that a new
//! command or type is added, or command proceeds in a different manner than before.
//! It can also signify changes in the `Header` message.
//!
//! Note, changes in syntax of Messages of a particular Application should be reflected
//! in `Header.content_type` field or the Content Message should implicitly support
//! versioning (ie. include `content_version` field for example).
//!
//! MAM message syntax version is indicated as the first tryte in the trinary encoded message.
//!
//! Backwards compatibility of the MAM implementations is welcome and not mandatory.

use iota_mam_protobuf3 as protobuf3;

/// MAM1 version number.
pub const MAM_1_VER: protobuf3::Trint3 = protobuf3::Trint3(0);

/// MAM1.1 version number.
pub const MAM_1_1_VER: protobuf3::Trint3 = protobuf3::Trint3(1);