pub(all) enum Capability {
  CAPABILITY_UNSPECIFIED
  CAPABILITY_CAPABILITIES
  CAPABILITY_REQUIRED_RESOURCES
  CAPABILITY_CREDENTIALS
  CAPABILITY_CONDITIONS
  CAPABILITY_REQUIRED_SCHEMAS
  Unknown(@lib.Enum)
} derive(Eq)
pub fn Capability::to_enum(self : Capability) -> @lib.Enum {
  match self {
    Capability::CAPABILITY_UNSPECIFIED => 0
    Capability::CAPABILITY_CAPABILITIES => 1
    Capability::CAPABILITY_REQUIRED_RESOURCES => 2
    Capability::CAPABILITY_CREDENTIALS => 3
    Capability::CAPABILITY_CONDITIONS => 4
    Capability::CAPABILITY_REQUIRED_SCHEMAS => 5
    Capability::Unknown(value) => value
  }
}
pub fn Capability::from_enum(i : @lib.Enum) -> Capability {
  match i.0 {
    0 => Capability::CAPABILITY_UNSPECIFIED
    1 => Capability::CAPABILITY_CAPABILITIES
    2 => Capability::CAPABILITY_REQUIRED_RESOURCES
    3 => Capability::CAPABILITY_CREDENTIALS
    4 => Capability::CAPABILITY_CONDITIONS
    5 => Capability::CAPABILITY_REQUIRED_SCHEMAS
    _ => Capability::Unknown(i)
  }
}
pub impl Default for Capability with fn default() -> Capability {
  Capability::CAPABILITY_UNSPECIFIED
}
pub impl @lib.Sized for Capability with fn size_of(self : Capability) {
  @lib.Sized::size_of(self.to_enum())
}
pub(all) enum Ready {
  READY_UNSPECIFIED
  READY_TRUE
  READY_FALSE
  Unknown(@lib.Enum)
} derive(Eq)
pub fn Ready::to_enum(self : Ready) -> @lib.Enum {
  match self {
    Ready::READY_UNSPECIFIED => 0
    Ready::READY_TRUE => 1
    Ready::READY_FALSE => 2
    Ready::Unknown(value) => value
  }
}
pub fn Ready::from_enum(i : @lib.Enum) -> Ready {
  match i.0 {
    0 => Ready::READY_UNSPECIFIED
    1 => Ready::READY_TRUE
    2 => Ready::READY_FALSE
    _ => Ready::Unknown(i)
  }
}
pub impl Default for Ready with fn default() -> Ready {
  Ready::READY_UNSPECIFIED
}
pub impl @lib.Sized for Ready with fn size_of(self : Ready) {
  @lib.Sized::size_of(self.to_enum())
}
pub(all) enum Severity {
  SEVERITY_UNSPECIFIED
  SEVERITY_FATAL
  SEVERITY_WARNING
  SEVERITY_NORMAL
  Unknown(@lib.Enum)
} derive(Eq)
pub fn Severity::to_enum(self : Severity) -> @lib.Enum {
  match self {
    Severity::SEVERITY_UNSPECIFIED => 0
    Severity::SEVERITY_FATAL => 1
    Severity::SEVERITY_WARNING => 2
    Severity::SEVERITY_NORMAL => 3
    Severity::Unknown(value) => value
  }
}
pub fn Severity::from_enum(i : @lib.Enum) -> Severity {
  match i.0 {
    0 => Severity::SEVERITY_UNSPECIFIED
    1 => Severity::SEVERITY_FATAL
    2 => Severity::SEVERITY_WARNING
    3 => Severity::SEVERITY_NORMAL
    _ => Severity::Unknown(i)
  }
}
pub impl Default for Severity with fn default() -> Severity {
  Severity::SEVERITY_UNSPECIFIED
}
pub impl @lib.Sized for Severity with fn size_of(self : Severity) {
  @lib.Sized::size_of(self.to_enum())
}
pub(all) enum Target {
  TARGET_UNSPECIFIED
  TARGET_COMPOSITE
  TARGET_COMPOSITE_AND_CLAIM
  Unknown(@lib.Enum)
} derive(Eq)
pub fn Target::to_enum(self : Target) -> @lib.Enum {
  match self {
    Target::TARGET_UNSPECIFIED => 0
    Target::TARGET_COMPOSITE => 1
    Target::TARGET_COMPOSITE_AND_CLAIM => 2
    Target::Unknown(value) => value
  }
}
pub fn Target::from_enum(i : @lib.Enum) -> Target {
  match i.0 {
    0 => Target::TARGET_UNSPECIFIED
    1 => Target::TARGET_COMPOSITE
    2 => Target::TARGET_COMPOSITE_AND_CLAIM
    _ => Target::Unknown(i)
  }
}
pub impl Default for Target with fn default() -> Target {
  Target::TARGET_UNSPECIFIED
}
pub impl @lib.Sized for Target with fn size_of(self : Target) {
  @lib.Sized::size_of(self.to_enum())
}
pub(all) enum Status {
  STATUS_CONDITION_UNSPECIFIED
  STATUS_CONDITION_UNKNOWN
  STATUS_CONDITION_TRUE
  STATUS_CONDITION_FALSE
  Unknown(@lib.Enum)
} derive(Eq)
pub fn Status::to_enum(self : Status) -> @lib.Enum {
  match self {
    Status::STATUS_CONDITION_UNSPECIFIED => 0
    Status::STATUS_CONDITION_UNKNOWN => 1
    Status::STATUS_CONDITION_TRUE => 2
    Status::STATUS_CONDITION_FALSE => 3
    Status::Unknown(value) => value
  }
}
pub fn Status::from_enum(i : @lib.Enum) -> Status {
  match i.0 {
    0 => Status::STATUS_CONDITION_UNSPECIFIED
    1 => Status::STATUS_CONDITION_UNKNOWN
    2 => Status::STATUS_CONDITION_TRUE
    3 => Status::STATUS_CONDITION_FALSE
    _ => Status::Unknown(i)
  }
}
pub impl Default for Status with fn default() -> Status {
  Status::STATUS_CONDITION_UNSPECIFIED
}
pub impl @lib.Sized for Status with fn size_of(self : Status) {
  @lib.Sized::size_of(self.to_enum())
}
pub(all) struct RunFunctionRequest_ExtraResourcesEntry {
  mut key : String
  mut value : Resources 
} derive(Eq)
pub impl @lib.Sized for RunFunctionRequest_ExtraResourcesEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for RunFunctionRequest_ExtraResourcesEntry with fn default() -> RunFunctionRequest_ExtraResourcesEntry {
  RunFunctionRequest_ExtraResourcesEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn RunFunctionRequest_ExtraResourcesEntry::RunFunctionRequest_ExtraResourcesEntry(key : String, value : Resources) -> RunFunctionRequest_ExtraResourcesEntry {
  RunFunctionRequest_ExtraResourcesEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for RunFunctionRequest_ExtraResourcesEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> RunFunctionRequest_ExtraResourcesEntry raise {
  let msg : RunFunctionRequest_ExtraResourcesEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : Resources)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for RunFunctionRequest_ExtraResourcesEntry with fn write(self: RunFunctionRequest_ExtraResourcesEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct RunFunctionRequest_CredentialsEntry {
  mut key : String
  mut value : Credentials 
} derive(Eq)
pub impl @lib.Sized for RunFunctionRequest_CredentialsEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for RunFunctionRequest_CredentialsEntry with fn default() -> RunFunctionRequest_CredentialsEntry {
  RunFunctionRequest_CredentialsEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn RunFunctionRequest_CredentialsEntry::RunFunctionRequest_CredentialsEntry(key : String, value : Credentials) -> RunFunctionRequest_CredentialsEntry {
  RunFunctionRequest_CredentialsEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for RunFunctionRequest_CredentialsEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> RunFunctionRequest_CredentialsEntry raise {
  let msg : RunFunctionRequest_CredentialsEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : Credentials)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for RunFunctionRequest_CredentialsEntry with fn write(self: RunFunctionRequest_CredentialsEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct RunFunctionRequest_RequiredResourcesEntry {
  mut key : String
  mut value : Resources 
} derive(Eq)
pub impl @lib.Sized for RunFunctionRequest_RequiredResourcesEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for RunFunctionRequest_RequiredResourcesEntry with fn default() -> RunFunctionRequest_RequiredResourcesEntry {
  RunFunctionRequest_RequiredResourcesEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn RunFunctionRequest_RequiredResourcesEntry::RunFunctionRequest_RequiredResourcesEntry(key : String, value : Resources) -> RunFunctionRequest_RequiredResourcesEntry {
  RunFunctionRequest_RequiredResourcesEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for RunFunctionRequest_RequiredResourcesEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> RunFunctionRequest_RequiredResourcesEntry raise {
  let msg : RunFunctionRequest_RequiredResourcesEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : Resources)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for RunFunctionRequest_RequiredResourcesEntry with fn write(self: RunFunctionRequest_RequiredResourcesEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct RunFunctionRequest_RequiredSchemasEntry {
  mut key : String
  mut value : Schema 
} derive(Eq)
pub impl @lib.Sized for RunFunctionRequest_RequiredSchemasEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for RunFunctionRequest_RequiredSchemasEntry with fn default() -> RunFunctionRequest_RequiredSchemasEntry {
  RunFunctionRequest_RequiredSchemasEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn RunFunctionRequest_RequiredSchemasEntry::RunFunctionRequest_RequiredSchemasEntry(key : String, value : Schema) -> RunFunctionRequest_RequiredSchemasEntry {
  RunFunctionRequest_RequiredSchemasEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for RunFunctionRequest_RequiredSchemasEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> RunFunctionRequest_RequiredSchemasEntry raise {
  let msg : RunFunctionRequest_RequiredSchemasEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : Schema)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for RunFunctionRequest_RequiredSchemasEntry with fn write(self: RunFunctionRequest_RequiredSchemasEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct RunFunctionRequest {
  mut meta : RequestMeta?
  mut observed : State?
  mut desired : State?
  mut input : @protobuf.Struct?
  mut context : @protobuf.Struct?
  mut extra_resources : Map[String, Resources]
  mut credentials : Map[String, Credentials]
  mut required_resources : Map[String, Resources]
  mut required_schemas : Map[String, Schema]
} derive(Eq)
pub impl @lib.Sized for RunFunctionRequest with fn size_of(self) {
  let mut size = 0U
  if self.meta is Some(v) {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(v))
  }
  if self.observed is Some(v) {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
  }
  if self.desired is Some(v) {
    size += @lib.size_of_length_delimited_field(3U, @lib.size_of(v))
  }
  if self.input is Some(v) {
    size += @lib.size_of_length_delimited_field(4U, @lib.size_of(v))
  }
  if self.context is Some(v) {
    size += @lib.size_of_length_delimited_field(5U, @lib.size_of(v))
  }
  for k, v in self.extra_resources {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(6U, key_size + value_size)
  }
  for k, v in self.credentials {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(7U, key_size + value_size)
  }
  for k, v in self.required_resources {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(8U, key_size + value_size)
  }
  for k, v in self.required_schemas {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(9U, key_size + value_size)
  }
  size
}
pub impl Default for RunFunctionRequest with fn default() -> RunFunctionRequest {
  RunFunctionRequest::{
    meta : None,
    observed : None,
    desired : None,
    input : None,
    context : None,
    extra_resources : {},
    credentials : {},
    required_resources : {},
    required_schemas : {},
  }
}
#alias(new, deprecated)
pub fn RunFunctionRequest::RunFunctionRequest(meta? : RequestMeta, observed? : State, desired? : State, input? : @protobuf.Struct, context? : @protobuf.Struct, extra_resources : Map[String, Resources], credentials : Map[String, Credentials], required_resources : Map[String, Resources], required_schemas : Map[String, Schema]) -> RunFunctionRequest {
  RunFunctionRequest::{
    meta,
    observed,
    desired,
    input,
    context,
    extra_resources,
    credentials,
    required_resources,
    required_schemas,
  }
}
pub impl @lib.Read for RunFunctionRequest with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> RunFunctionRequest raise {
  let msg : RunFunctionRequest = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.meta = (reader |> @lib.read_message() : RequestMeta) |> Some
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.observed = (reader |> @lib.read_message() : State) |> Some
      (3, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.desired = (reader |> @lib.read_message() : State) |> Some
      (4, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.input = (reader |> @lib.read_message() : @protobuf.Struct) |> Some
      (5, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.context = (reader |> @lib.read_message() : @protobuf.Struct) |> Some
      (6, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : RunFunctionRequest_ExtraResourcesEntry); msg.extra_resources[key] = value }
      (7, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : RunFunctionRequest_CredentialsEntry); msg.credentials[key] = value }
      (8, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : RunFunctionRequest_RequiredResourcesEntry); msg.required_resources[key] = value }
      (9, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : RunFunctionRequest_RequiredSchemasEntry); msg.required_schemas[key] = value }
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for RunFunctionRequest with fn write(self: RunFunctionRequest, writer : &@lib.Writer) -> Unit raise {
  if self.meta is Some(v) {
    writer |> @lib.write_varint(10UL);
    writer |> @lib.write_message(v)
  }
  if self.observed is Some(v) {
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
  if self.desired is Some(v) {
    writer |> @lib.write_varint(26UL);
    writer |> @lib.write_message(v)
  }
  if self.input is Some(v) {
    writer |> @lib.write_varint(34UL);
    writer |> @lib.write_message(v)
  }
  if self.context is Some(v) {
    writer |> @lib.write_varint(42UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.extra_resources.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.extra_resources.get(k).unwrap()
    writer |> @lib.write_varint(50UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.credentials.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.credentials.get(k).unwrap()
    writer |> @lib.write_varint(58UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.required_resources.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.required_resources.get(k).unwrap()
    writer |> @lib.write_varint(66UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.required_schemas.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.required_schemas.get(k).unwrap()
    writer |> @lib.write_varint(74UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
}
pub(all) struct Credentials {
  mut source : Credentials_Source
} derive(Eq)
pub(all) enum Credentials_Source {
  CredentialData(CredentialData)
  NotSet
} derive(Eq)
pub impl Default for Credentials_Source with fn default() -> Credentials_Source {
  NotSet
}
pub impl @lib.Sized for Credentials with fn size_of(self) {
  let mut size = 0U
  match self.source {
    CredentialData(v) => { size += @lib.size_of_length_delimited_field(1U, @lib.size_of(v)) }
    NotSet => ()
  }
  size
}
pub impl Default for Credentials with fn default() -> Credentials {
  Credentials::{
    source : Credentials_Source::NotSet,
  }
}
#alias(new, deprecated)
pub fn Credentials::Credentials(source?: Credentials_Source = Credentials_Source::NotSet) -> Credentials {
  Credentials::{
    source,
  }
}
pub impl @lib.Read for Credentials with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Credentials raise {
  let msg : Credentials = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.source = (reader |> @lib.read_message() : CredentialData) |> Credentials_Source::CredentialData
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Credentials with fn write(self: Credentials, writer : &@lib.Writer) -> Unit raise {
  match self.source {
    Credentials_Source::CredentialData(v) => {
      writer |> @lib.write_varint(10UL)
      writer |> @lib.write_message(v)
     }
    Credentials_Source::NotSet => ()
  }
}
pub(all) struct CredentialData_DataEntry {
  mut key : String
  mut value : Bytes
} derive(Eq)
pub impl @lib.Sized for CredentialData_DataEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  if self.value != Default::default() {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  }
  size
}
pub impl Default for CredentialData_DataEntry with fn default() -> CredentialData_DataEntry {
  CredentialData_DataEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn CredentialData_DataEntry::CredentialData_DataEntry(key : String, value : Bytes) -> CredentialData_DataEntry {
  CredentialData_DataEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for CredentialData_DataEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> CredentialData_DataEntry raise {
  let msg : CredentialData_DataEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, _) => msg.value = reader |> @lib.read_bytes()
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for CredentialData_DataEntry with fn write(self: CredentialData_DataEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  if self.value != Default::default() {
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_bytes(self.value)
  }
}
pub(all) struct CredentialData {
  mut data : Map[String, Bytes]
} derive(Eq)
pub impl @lib.Sized for CredentialData with fn size_of(self) {
  let mut size = 0U
  for k, v in self.data {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(1U, key_size + value_size)
  }
  size
}
pub impl Default for CredentialData with fn default() -> CredentialData {
  CredentialData::{
    data : {},
  }
}
#alias(new, deprecated)
pub fn CredentialData::CredentialData(data : Map[String, Bytes]) -> CredentialData {
  CredentialData::{
    data,
  }
}
pub impl @lib.Read for CredentialData with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> CredentialData raise {
  let msg : CredentialData = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : CredentialData_DataEntry); msg.data[key] = value }
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for CredentialData with fn write(self: CredentialData, writer : &@lib.Writer) -> Unit raise {
  let keys = self.data.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.data.get(k).unwrap()
    writer |> @lib.write_varint(10UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_bytes(v)
  }
}
pub(all) struct Resources {
  mut items : Array[Resource]
} derive(Eq)
pub impl @lib.Sized for Resources with fn size_of(self) {
  let mut size = 0U
  for s in self.items {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(s))
  }
  size
}
pub impl Default for Resources with fn default() -> Resources {
  Resources::{
    items : [],
  }
}
#alias(new, deprecated)
pub fn Resources::Resources(items : Array[Resource]) -> Resources {
  Resources::{
    items,
  }
}
pub impl @lib.Read for Resources with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Resources raise {
  let msg : Resources = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.items.push((reader |> @lib.read_message() : Resource))
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Resources with fn write(self: Resources, writer : &@lib.Writer) -> Unit raise {
  for item in self.items {
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_message(item)
  }
}
pub(all) struct RunFunctionResponse {
  mut meta : ResponseMeta?
  mut desired : State?
  mut results : Array[Result]
  mut context : @protobuf.Struct?
  mut requirements : Requirements?
  mut conditions : Array[Condition]
  mut output : @protobuf.Struct?
} derive(Eq)
pub impl @lib.Sized for RunFunctionResponse with fn size_of(self) {
  let mut size = 0U
  if self.meta is Some(v) {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(v))
  }
  if self.desired is Some(v) {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
  }
  for s in self.results {
    size += @lib.size_of_length_delimited_field(3U, @lib.size_of(s))
  }
  if self.context is Some(v) {
    size += @lib.size_of_length_delimited_field(4U, @lib.size_of(v))
  }
  if self.requirements is Some(v) {
    size += @lib.size_of_length_delimited_field(5U, @lib.size_of(v))
  }
  for s in self.conditions {
    size += @lib.size_of_length_delimited_field(6U, @lib.size_of(s))
  }
  if self.output is Some(v) {
    size += @lib.size_of_length_delimited_field(7U, @lib.size_of(v))
  }
  size
}
pub impl Default for RunFunctionResponse with fn default() -> RunFunctionResponse {
  RunFunctionResponse::{
    meta : None,
    desired : None,
    results : [],
    context : None,
    requirements : None,
    conditions : [],
    output : None,
  }
}
#alias(new, deprecated)
pub fn RunFunctionResponse::RunFunctionResponse(meta? : ResponseMeta, desired? : State, results : Array[Result], context? : @protobuf.Struct, requirements? : Requirements, conditions : Array[Condition], output? : @protobuf.Struct) -> RunFunctionResponse {
  RunFunctionResponse::{
    meta,
    desired,
    results,
    context,
    requirements,
    conditions,
    output,
  }
}
pub impl @lib.Read for RunFunctionResponse with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> RunFunctionResponse raise {
  let msg : RunFunctionResponse = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.meta = (reader |> @lib.read_message() : ResponseMeta) |> Some
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.desired = (reader |> @lib.read_message() : State) |> Some
      (3, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.results.push((reader |> @lib.read_message() : Result))
      (4, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.context = (reader |> @lib.read_message() : @protobuf.Struct) |> Some
      (5, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.requirements = (reader |> @lib.read_message() : Requirements) |> Some
      (6, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.conditions.push((reader |> @lib.read_message() : Condition))
      (7, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.output = (reader |> @lib.read_message() : @protobuf.Struct) |> Some
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for RunFunctionResponse with fn write(self: RunFunctionResponse, writer : &@lib.Writer) -> Unit raise {
  if self.meta is Some(v) {
    writer |> @lib.write_varint(10UL);
    writer |> @lib.write_message(v)
  }
  if self.desired is Some(v) {
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
  for item in self.results {
    writer |> @lib.write_varint(26UL)
    writer |> @lib.write_message(item)
  }
  if self.context is Some(v) {
    writer |> @lib.write_varint(34UL);
    writer |> @lib.write_message(v)
  }
  if self.requirements is Some(v) {
    writer |> @lib.write_varint(42UL);
    writer |> @lib.write_message(v)
  }
  for item in self.conditions {
    writer |> @lib.write_varint(50UL)
    writer |> @lib.write_message(item)
  }
  if self.output is Some(v) {
    writer |> @lib.write_varint(58UL);
    writer |> @lib.write_message(v)
  }
}
pub(all) struct RequestMeta {
  mut tag : String
  mut capabilities : Array[Capability]
} derive(Eq)
pub impl @lib.Sized for RequestMeta with fn size_of(self) {
  let mut size = 0U
  if self.tag != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.tag))
  }
  if !self.capabilities.is_empty() {
    size += @lib.size_of_length_delimited_field(2U, self.capabilities.iter().map(@lib.size_of).fold(init=0U, UInt::add))
  }
  size
}
pub impl Default for RequestMeta with fn default() -> RequestMeta {
  RequestMeta::{
    tag : Default::default(),
    capabilities : [],
  }
}
#alias(new, deprecated)
pub fn RequestMeta::RequestMeta(tag : String, capabilities : Array[Capability]) -> RequestMeta {
  RequestMeta::{
    tag,
    capabilities,
  }
}
pub impl @lib.Read for RequestMeta with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> RequestMeta raise {
  let msg : RequestMeta = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.tag = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let packed = reader |> @lib.read_packed(@lib.read_enum, None); for item in packed { msg.capabilities.push(Capability::from_enum(item)) } }
      (2, @lib.WIRE_TYPE_VARINT) => msg.capabilities.push(reader |> @lib.read_enum() |> Capability::from_enum)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for RequestMeta with fn write(self: RequestMeta, writer : &@lib.Writer) -> Unit raise {
  if self.tag != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.tag)
  }
  if !self.capabilities.is_empty() {
    writer |> @lib.write_varint(18UL)
    let size = self.capabilities.iter().map(@lib.size_of).fold(init=0U, UInt::add)
    writer |> @lib.write_uint32(size)
    for item in self.capabilities {
        writer |> @lib.write_enum(item.to_enum())
    }
  }
}
pub(all) struct Requirements_ExtraResourcesEntry {
  mut key : String
  mut value : ResourceSelector 
} derive(Eq)
pub impl @lib.Sized for Requirements_ExtraResourcesEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for Requirements_ExtraResourcesEntry with fn default() -> Requirements_ExtraResourcesEntry {
  Requirements_ExtraResourcesEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn Requirements_ExtraResourcesEntry::Requirements_ExtraResourcesEntry(key : String, value : ResourceSelector) -> Requirements_ExtraResourcesEntry {
  Requirements_ExtraResourcesEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for Requirements_ExtraResourcesEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Requirements_ExtraResourcesEntry raise {
  let msg : Requirements_ExtraResourcesEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : ResourceSelector)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Requirements_ExtraResourcesEntry with fn write(self: Requirements_ExtraResourcesEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct Requirements_ResourcesEntry {
  mut key : String
  mut value : ResourceSelector 
} derive(Eq)
pub impl @lib.Sized for Requirements_ResourcesEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for Requirements_ResourcesEntry with fn default() -> Requirements_ResourcesEntry {
  Requirements_ResourcesEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn Requirements_ResourcesEntry::Requirements_ResourcesEntry(key : String, value : ResourceSelector) -> Requirements_ResourcesEntry {
  Requirements_ResourcesEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for Requirements_ResourcesEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Requirements_ResourcesEntry raise {
  let msg : Requirements_ResourcesEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : ResourceSelector)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Requirements_ResourcesEntry with fn write(self: Requirements_ResourcesEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct Requirements_SchemasEntry {
  mut key : String
  mut value : SchemaSelector 
} derive(Eq)
pub impl @lib.Sized for Requirements_SchemasEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for Requirements_SchemasEntry with fn default() -> Requirements_SchemasEntry {
  Requirements_SchemasEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn Requirements_SchemasEntry::Requirements_SchemasEntry(key : String, value : SchemaSelector) -> Requirements_SchemasEntry {
  Requirements_SchemasEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for Requirements_SchemasEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Requirements_SchemasEntry raise {
  let msg : Requirements_SchemasEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : SchemaSelector)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Requirements_SchemasEntry with fn write(self: Requirements_SchemasEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct Requirements {
  mut extra_resources : Map[String, ResourceSelector]
  mut resources : Map[String, ResourceSelector]
  mut schemas : Map[String, SchemaSelector]
} derive(Eq)
pub impl @lib.Sized for Requirements with fn size_of(self) {
  let mut size = 0U
  for k, v in self.extra_resources {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(1U, key_size + value_size)
  }
  for k, v in self.resources {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(2U, key_size + value_size)
  }
  for k, v in self.schemas {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(3U, key_size + value_size)
  }
  size
}
pub impl Default for Requirements with fn default() -> Requirements {
  Requirements::{
    extra_resources : {},
    resources : {},
    schemas : {},
  }
}
#alias(new, deprecated)
pub fn Requirements::Requirements(extra_resources : Map[String, ResourceSelector], resources : Map[String, ResourceSelector], schemas : Map[String, SchemaSelector]) -> Requirements {
  Requirements::{
    extra_resources,
    resources,
    schemas,
  }
}
pub impl @lib.Read for Requirements with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Requirements raise {
  let msg : Requirements = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : Requirements_ExtraResourcesEntry); msg.extra_resources[key] = value }
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : Requirements_ResourcesEntry); msg.resources[key] = value }
      (3, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : Requirements_SchemasEntry); msg.schemas[key] = value }
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Requirements with fn write(self: Requirements, writer : &@lib.Writer) -> Unit raise {
  let keys = self.extra_resources.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.extra_resources.get(k).unwrap()
    writer |> @lib.write_varint(10UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.resources.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.resources.get(k).unwrap()
    writer |> @lib.write_varint(18UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.schemas.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.schemas.get(k).unwrap()
    writer |> @lib.write_varint(26UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
}
pub(all) struct SchemaSelector {
  mut api_version : String
  mut kind : String
} derive(Eq)
pub impl @lib.Sized for SchemaSelector with fn size_of(self) {
  let mut size = 0U
  if self.api_version != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.api_version))
  }
  if self.kind != Default::default() {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.kind))
  }
  size
}
pub impl Default for SchemaSelector with fn default() -> SchemaSelector {
  SchemaSelector::{
    api_version : Default::default(),
    kind : Default::default(),
  }
}
#alias(new, deprecated)
pub fn SchemaSelector::SchemaSelector(api_version : String, kind : String) -> SchemaSelector {
  SchemaSelector::{
    api_version,
    kind,
  }
}
pub impl @lib.Read for SchemaSelector with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> SchemaSelector raise {
  let msg : SchemaSelector = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.api_version = reader |> @lib.read_string()
      (2, _) => msg.kind = reader |> @lib.read_string()
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for SchemaSelector with fn write(self: SchemaSelector, writer : &@lib.Writer) -> Unit raise {
  if self.api_version != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.api_version)
  }
  if self.kind != Default::default() {
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_string(self.kind)
  }
}
pub(all) struct Schema {
  mut openapi_v3 : @protobuf.Struct?
} derive(Eq)
pub impl @lib.Sized for Schema with fn size_of(self) {
  let mut size = 0U
  if self.openapi_v3 is Some(v) {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(v))
  }
  size
}
pub impl Default for Schema with fn default() -> Schema {
  Schema::{
    openapi_v3 : None,
  }
}
#alias(new, deprecated)
pub fn Schema::Schema(openapi_v3? : @protobuf.Struct) -> Schema {
  Schema::{
    openapi_v3,
  }
}
pub impl @lib.Read for Schema with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Schema raise {
  let msg : Schema = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.openapi_v3 = (reader |> @lib.read_message() : @protobuf.Struct) |> Some
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Schema with fn write(self: Schema, writer : &@lib.Writer) -> Unit raise {
  if self.openapi_v3 is Some(v) {
    writer |> @lib.write_varint(10UL);
    writer |> @lib.write_message(v)
  }
}
pub(all) struct ResourceSelector {
  mut api_version : String
  mut kind : String
  mut namespace_ : String?
  mut match_ : ResourceSelector_Match
} derive(Eq)
pub(all) enum ResourceSelector_Match {
  MatchName(String)
  MatchLabels(MatchLabels)
  NotSet
} derive(Eq)
pub impl Default for ResourceSelector_Match with fn default() -> ResourceSelector_Match {
  NotSet
}
pub impl @lib.Sized for ResourceSelector with fn size_of(self) {
  let mut size = 0U
  if self.api_version != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.api_version))
  }
  if self.kind != Default::default() {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.kind))
  }
  if self.namespace_ is Some(v) {
    size += @lib.size_of_length_delimited_field(5U, @lib.size_of(v))
  }
  match self.match_ {
    MatchName(v) => { size += @lib.size_of_length_delimited_field(3U, @lib.size_of(v)) }
    MatchLabels(v) => { size += @lib.size_of_length_delimited_field(4U, @lib.size_of(v)) }
    NotSet => ()
  }
  size
}
pub impl Default for ResourceSelector with fn default() -> ResourceSelector {
  ResourceSelector::{
    api_version : Default::default(),
    kind : Default::default(),
    namespace_ : None,
    match_ : ResourceSelector_Match::NotSet,
  }
}
#alias(new, deprecated)
pub fn ResourceSelector::ResourceSelector(api_version : String, kind : String, namespace_? : String, match_?: ResourceSelector_Match = ResourceSelector_Match::NotSet) -> ResourceSelector {
  ResourceSelector::{
    api_version,
    kind,
    namespace_,
    match_,
  }
}
pub impl @lib.Read for ResourceSelector with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> ResourceSelector raise {
  let msg : ResourceSelector = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.api_version = reader |> @lib.read_string()
      (2, _) => msg.kind = reader |> @lib.read_string()
      (5, _) => msg.namespace_ = reader |> @lib.read_string() |> Some
      (3, _) => msg.match_ = reader |> @lib.read_string() |> ResourceSelector_Match::MatchName
      (4, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.match_ = (reader |> @lib.read_message() : MatchLabels) |> ResourceSelector_Match::MatchLabels
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for ResourceSelector with fn write(self: ResourceSelector, writer : &@lib.Writer) -> Unit raise {
  if self.api_version != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.api_version)
  }
  if self.kind != Default::default() {
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_string(self.kind)
  }
  if self.namespace_ is Some(v) {
    writer |> @lib.write_varint(42UL);
    writer |> @lib.write_string(v)
  }
  match self.match_ {
    ResourceSelector_Match::MatchName(v) => {
      writer |> @lib.write_varint(26UL)
      writer |> @lib.write_string(v)
     }
    ResourceSelector_Match::MatchLabels(v) => {
      writer |> @lib.write_varint(34UL)
      writer |> @lib.write_message(v)
     }
    ResourceSelector_Match::NotSet => ()
  }
}
pub(all) struct MatchLabels_LabelsEntry {
  mut key : String
  mut value : String
} derive(Eq)
pub impl @lib.Sized for MatchLabels_LabelsEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  if self.value != Default::default() {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  }
  size
}
pub impl Default for MatchLabels_LabelsEntry with fn default() -> MatchLabels_LabelsEntry {
  MatchLabels_LabelsEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn MatchLabels_LabelsEntry::MatchLabels_LabelsEntry(key : String, value : String) -> MatchLabels_LabelsEntry {
  MatchLabels_LabelsEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for MatchLabels_LabelsEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> MatchLabels_LabelsEntry raise {
  let msg : MatchLabels_LabelsEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, _) => msg.value = reader |> @lib.read_string()
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for MatchLabels_LabelsEntry with fn write(self: MatchLabels_LabelsEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  if self.value != Default::default() {
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_string(self.value)
  }
}
pub(all) struct MatchLabels {
  mut labels : Map[String, String]
} derive(Eq)
pub impl @lib.Sized for MatchLabels with fn size_of(self) {
  let mut size = 0U
  for k, v in self.labels {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(1U, key_size + value_size)
  }
  size
}
pub impl Default for MatchLabels with fn default() -> MatchLabels {
  MatchLabels::{
    labels : {},
  }
}
#alias(new, deprecated)
pub fn MatchLabels::MatchLabels(labels : Map[String, String]) -> MatchLabels {
  MatchLabels::{
    labels,
  }
}
pub impl @lib.Read for MatchLabels with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> MatchLabels raise {
  let msg : MatchLabels = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : MatchLabels_LabelsEntry); msg.labels[key] = value }
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for MatchLabels with fn write(self: MatchLabels, writer : &@lib.Writer) -> Unit raise {
  let keys = self.labels.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.labels.get(k).unwrap()
    writer |> @lib.write_varint(10UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_string(v)
  }
}
pub(all) struct ResponseMeta {
  mut tag : String
  mut ttl : @protobuf.Duration?
} derive(Eq)
pub impl @lib.Sized for ResponseMeta with fn size_of(self) {
  let mut size = 0U
  if self.tag != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.tag))
  }
  if self.ttl is Some(v) {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
  }
  size
}
pub impl Default for ResponseMeta with fn default() -> ResponseMeta {
  ResponseMeta::{
    tag : Default::default(),
    ttl : None,
  }
}
#alias(new, deprecated)
pub fn ResponseMeta::ResponseMeta(tag : String, ttl? : @protobuf.Duration) -> ResponseMeta {
  ResponseMeta::{
    tag,
    ttl,
  }
}
pub impl @lib.Read for ResponseMeta with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> ResponseMeta raise {
  let msg : ResponseMeta = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.tag = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.ttl = (reader |> @lib.read_message() : @protobuf.Duration) |> Some
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for ResponseMeta with fn write(self: ResponseMeta, writer : &@lib.Writer) -> Unit raise {
  if self.tag != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.tag)
  }
  if self.ttl is Some(v) {
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
}
pub(all) struct State_ResourcesEntry {
  mut key : String
  mut value : Resource 
} derive(Eq)
pub impl @lib.Sized for State_ResourcesEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  size
}
pub impl Default for State_ResourcesEntry with fn default() -> State_ResourcesEntry {
  State_ResourcesEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn State_ResourcesEntry::State_ResourcesEntry(key : String, value : Resource) -> State_ResourcesEntry {
  State_ResourcesEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for State_ResourcesEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> State_ResourcesEntry raise {
  let msg : State_ResourcesEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.value = (reader |> @lib.read_message() : Resource)
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for State_ResourcesEntry with fn write(self: State_ResourcesEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_message(self.value)
}
pub(all) struct State {
  mut composite : Resource?
  mut resources : Map[String, Resource]
} derive(Eq)
pub impl @lib.Sized for State with fn size_of(self) {
  let mut size = 0U
  if self.composite is Some(v) {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(v))
  }
  for k, v in self.resources {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(2U, key_size + value_size)
  }
  size
}
pub impl Default for State with fn default() -> State {
  State::{
    composite : None,
    resources : {},
  }
}
#alias(new, deprecated)
pub fn State::State(composite? : Resource, resources : Map[String, Resource]) -> State {
  State::{
    composite,
    resources,
  }
}
pub impl @lib.Read for State with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> State raise {
  let msg : State = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.composite = (reader |> @lib.read_message() : Resource) |> Some
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : State_ResourcesEntry); msg.resources[key] = value }
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for State with fn write(self: State, writer : &@lib.Writer) -> Unit raise {
  if self.composite is Some(v) {
    writer |> @lib.write_varint(10UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.resources.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.resources.get(k).unwrap()
    writer |> @lib.write_varint(18UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_message(v)
  }
}
pub(all) struct Resource_ConnectionDetailsEntry {
  mut key : String
  mut value : Bytes
} derive(Eq)
pub impl @lib.Sized for Resource_ConnectionDetailsEntry with fn size_of(self) {
  let mut size = 0U
  if self.key != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.key))
  }
  if self.value != Default::default() {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.value))
  }
  size
}
pub impl Default for Resource_ConnectionDetailsEntry with fn default() -> Resource_ConnectionDetailsEntry {
  Resource_ConnectionDetailsEntry::{
    key : Default::default(),
    value : Default::default(),
  }
}
#alias(new, deprecated)
pub fn Resource_ConnectionDetailsEntry::Resource_ConnectionDetailsEntry(key : String, value : Bytes) -> Resource_ConnectionDetailsEntry {
  Resource_ConnectionDetailsEntry::{
    key,
    value,
  }
}
pub impl @lib.Read for Resource_ConnectionDetailsEntry with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Resource_ConnectionDetailsEntry raise {
  let msg : Resource_ConnectionDetailsEntry = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.key = reader |> @lib.read_string()
      (2, _) => msg.value = reader |> @lib.read_bytes()
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Resource_ConnectionDetailsEntry with fn write(self: Resource_ConnectionDetailsEntry, writer : &@lib.Writer) -> Unit raise {
  if self.key != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.key)
  }
  if self.value != Default::default() {
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_bytes(self.value)
  }
}
pub(all) struct Resource {
  mut resource : @protobuf.Struct?
  mut connection_details : Map[String, Bytes]
  mut ready : Ready 
} derive(Eq)
pub impl @lib.Sized for Resource with fn size_of(self) {
  let mut size = 0U
  if self.resource is Some(v) {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(v))
  }
  for k, v in self.connection_details {
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    size += @lib.size_of_length_delimited_field(2U, key_size + value_size)
  }
  if self.ready != Default::default() {
    size += @lib.size_of_field(3U, @lib.size_of(self.ready))
  }
  size
}
pub impl Default for Resource with fn default() -> Resource {
  Resource::{
    resource : None,
    connection_details : {},
    ready : Default::default(),
  }
}
#alias(new, deprecated)
pub fn Resource::Resource(resource? : @protobuf.Struct, connection_details : Map[String, Bytes], ready : Ready) -> Resource {
  Resource::{
    resource,
    connection_details,
    ready,
  }
}
pub impl @lib.Read for Resource with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Resource raise {
  let msg : Resource = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, @lib.WIRE_TYPE_LENGTH_DELIMITED) => msg.resource = (reader |> @lib.read_message() : @protobuf.Struct) |> Some
      (2, @lib.WIRE_TYPE_LENGTH_DELIMITED) => { let {key, value} = (reader |> @lib.read_message() : Resource_ConnectionDetailsEntry); msg.connection_details[key] = value }
      (3, _) => msg.ready = reader |> @lib.read_enum() |> Ready::from_enum
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Resource with fn write(self: Resource, writer : &@lib.Writer) -> Unit raise {
  if self.resource is Some(v) {
    writer |> @lib.write_varint(10UL);
    writer |> @lib.write_message(v)
  }
  let keys = self.connection_details.keys().collect()
  for i in 0..<keys.length() {
    let k = keys[i]
    let v = self.connection_details.get(k).unwrap()
    writer |> @lib.write_varint(18UL)
    let key_size = @lib.size_of_length_delimited_field(1U, @lib.size_of(k))
    let value_size = @lib.size_of_length_delimited_field(2U, @lib.size_of(v))
    writer |> @lib.write_uint32(key_size + value_size)
    writer |> @lib.write_varint(10UL)
    writer |> @lib.write_string(k)
    writer |> @lib.write_varint(18UL);
    writer |> @lib.write_bytes(v)
  }
  if self.ready != Default::default() {
  writer |> @lib.write_varint(24UL);
  writer |> @lib.write_enum(self.ready.to_enum())
  }
}
pub(all) struct Result {
  mut severity : Severity 
  mut message : String
  mut reason : String?
  mut target : Target?
} derive(Eq)
pub impl @lib.Sized for Result with fn size_of(self) {
  let mut size = 0U
  if self.severity != Default::default() {
    size += @lib.size_of_field(1U, @lib.size_of(self.severity))
  }
  if self.message != Default::default() {
    size += @lib.size_of_length_delimited_field(2U, @lib.size_of(self.message))
  }
  if self.reason is Some(v) {
    size += @lib.size_of_length_delimited_field(3U, @lib.size_of(v))
  }
  if self.target is Some(v) {
    size += @lib.size_of_field(4U, @lib.size_of(v))
  }
  size
}
pub impl Default for Result with fn default() -> Result {
  Result::{
    severity : Default::default(),
    message : Default::default(),
    reason : None,
    target : None,
  }
}
#alias(new, deprecated)
pub fn Result::Result(severity : Severity, message : String, reason? : String, target? : Target) -> Result {
  Result::{
    severity,
    message,
    reason,
    target,
  }
}
pub impl @lib.Read for Result with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Result raise {
  let msg : Result = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.severity = reader |> @lib.read_enum() |> Severity::from_enum
      (2, _) => msg.message = reader |> @lib.read_string()
      (3, _) => msg.reason = reader |> @lib.read_string() |> Some
      (4, _) => msg.target = reader |> @lib.read_enum() |> Target::from_enum |> Some
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Result with fn write(self: Result, writer : &@lib.Writer) -> Unit raise {
  if self.severity != Default::default() {
  writer |> @lib.write_varint(8UL);
  writer |> @lib.write_enum(self.severity.to_enum())
  }
  if self.message != Default::default() {
  writer |> @lib.write_varint(18UL);
  writer |> @lib.write_string(self.message)
  }
  if self.reason is Some(v) {
    writer |> @lib.write_varint(26UL);
    writer |> @lib.write_string(v)
  }
  if self.target is Some(v) {
    writer |> @lib.write_varint(32UL);
    writer |> @lib.write_enum(v.to_enum())
  }
}
pub(all) struct Condition {
  mut type_ : String
  mut status : Status 
  mut reason : String
  mut message : String?
  mut target : Target?
} derive(Eq)
pub impl @lib.Sized for Condition with fn size_of(self) {
  let mut size = 0U
  if self.type_ != Default::default() {
    size += @lib.size_of_length_delimited_field(1U, @lib.size_of(self.type_))
  }
  if self.status != Default::default() {
    size += @lib.size_of_field(2U, @lib.size_of(self.status))
  }
  if self.reason != Default::default() {
    size += @lib.size_of_length_delimited_field(3U, @lib.size_of(self.reason))
  }
  if self.message is Some(v) {
    size += @lib.size_of_length_delimited_field(4U, @lib.size_of(v))
  }
  if self.target is Some(v) {
    size += @lib.size_of_field(5U, @lib.size_of(v))
  }
  size
}
pub impl Default for Condition with fn default() -> Condition {
  Condition::{
    type_ : Default::default(),
    status : Default::default(),
    reason : Default::default(),
    message : None,
    target : None,
  }
}
#alias(new, deprecated)
pub fn Condition::Condition(type_ : String, status : Status, reason : String, message? : String, target? : Target) -> Condition {
  Condition::{
    type_,
    status,
    reason,
    message,
    target,
  }
}
pub impl @lib.Read for Condition with fn read_with_limit(reader : @lib.LimitedReader[&@lib.Reader]) -> Condition raise {
  let msg : Condition = Default::default()
  while (reader |> @lib.read_next_message_field()) is Some((field_number, wire)) {
    match (field_number, wire) {
      (1, _) => msg.type_ = reader |> @lib.read_string()
      (2, _) => msg.status = reader |> @lib.read_enum() |> Status::from_enum
      (3, _) => msg.reason = reader |> @lib.read_string()
      (4, _) => msg.message = reader |> @lib.read_string() |> Some
      (5, _) => msg.target = reader |> @lib.read_enum() |> Target::from_enum |> Some
      _ => reader |> @lib.skip_message_field_by_number(field_number, wire)
    }
  }
  msg
}
pub impl @lib.Write for Condition with fn write(self: Condition, writer : &@lib.Writer) -> Unit raise {
  if self.type_ != Default::default() {
  writer |> @lib.write_varint(10UL);
  writer |> @lib.write_string(self.type_)
  }
  if self.status != Default::default() {
  writer |> @lib.write_varint(16UL);
  writer |> @lib.write_enum(self.status.to_enum())
  }
  if self.reason != Default::default() {
  writer |> @lib.write_varint(26UL);
  writer |> @lib.write_string(self.reason)
  }
  if self.message is Some(v) {
    writer |> @lib.write_varint(34UL);
    writer |> @lib.write_string(v)
  }
  if self.target is Some(v) {
    writer |> @lib.write_varint(40UL);
    writer |> @lib.write_enum(v.to_enum())
  }
}
