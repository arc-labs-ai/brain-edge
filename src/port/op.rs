//! The op vocabulary: every verb the HTTP surface meters, named once.
//!
//! [`crate::state::EdgeState`]'s metering methods take an [`Op`], not a
//! string, which is what makes the set closed: a handler cannot report an op
//! that is not declared here, and a declared op cannot be missing from
//! [`Op::ALL`] because one list generates both.
//!
//! That matters to whoever is on the other side of [`crate::port::MeteringSink`].
//! A host that bills or analyses per op has to know the whole set, and the
//! only alternative to reading it from here is keeping a copy — which drifts
//! the moment a verb is added, and an op the biller has never heard of is one
//! that gets served for free.
//!
//! The labels are the analytics vocabulary, so each is a contract: renaming
//! one splits a tenant's history across two names.
//!
//! Scope is the HTTP surface. [`crate::wire_proxy`] splices frames it does not
//! decode and reports a coarse opcode label of its own.

/// Declare the vocabulary once; derive the type, the labels and the full set
/// from it.
macro_rules! ops {
    ($( $variant:ident => $label:literal ),+ $(,)?) => {
        /// One op the HTTP surface serves, as metering and quota see it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Op {
            $(
                #[doc = concat!("The `", $label, "` op.")]
                $variant,
            )+
        }

        impl Op {
            /// Every op, in declaration order.
            ///
            /// The complete set a metering sink can be handed, for a host that
            /// needs to prove it has a rule for each one.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// The label this op is metered and billed under.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $label, )+ }
            }
        }
    };
}

ops! {
    // identity
    Whoami => "whoami",
    Capabilities => "capabilities",
    // memory
    Encode => "encode",
    EncodeVectorDirect => "encode_vector_direct",
    Recall => "recall",
    MemoryList => "memory_list",
    MemoryInspect => "memory_inspect",
    Forget => "forget",
    Subscribe => "subscribe",
    // reasoning
    Plan => "plan",
    Reason => "reason",
    // memory graph
    Link => "link",
    Unlink => "unlink",
    GraphFetch => "graph_fetch",
    // typed-graph entities
    CreateEntity => "create_entity",
    ListEntities => "list_entities",
    EntityTypeList => "entity_type_list",
    ResolveEntity => "resolve_entity",
    GetEntity => "get_entity",
    UpdateEntity => "update_entity",
    RenameEntity => "rename_entity",
    MergeEntities => "merge_entities",
    UnmergeEntity => "unmerge_entity",
    TombstoneEntity => "tombstone_entity",
    Traverse => "traverse",
    // typed-graph relations
    CreateRelation => "create_relation",
    ListRelations => "list_relations",
    GetRelation => "get_relation",
    SupersedeRelation => "supersede_relation",
    TombstoneRelation => "tombstone_relation",
    // typed-graph statements
    CreateStatement => "create_statement",
    ListStatements => "list_statements",
    GetStatement => "get_statement",
    SupersedeStatement => "supersede_statement",
    RetractStatement => "retract_statement",
    TombstoneStatement => "tombstone_statement",
    StatementHistory => "statement_history",
    // schema
    GetSchema => "get_schema",
    ListSchemas => "list_schemas",
    UploadSchema => "upload_schema",
    ReplaceSchema => "replace_schema",
    DropSchema => "drop_schema",
    ValidateSchema => "validate_schema",
    ExtractorList => "extractor_list",
    MaterializeProcedural => "materialize_procedural",
    // spaces
    CreateSpace => "create_space",
    ListSpaces => "list_spaces",
    DeleteSpace => "delete_space",
    // sessions
    CreateSession => "create_session",
    ListSessions => "list_sessions",
    DeleteSession => "delete_session",
    // query introspection
    QueryExplain => "query_explain",
    QueryTrace => "query_trace",
    // BYOK provider credentials
    LlmCredentialPut => "llm_credential_put",
    LlmCredentialGet => "llm_credential_get",
    LlmCredentialDelete => "llm_credential_delete",
    LlmCredentialTest => "llm_credential_test",
    LlmCredentialVerify => "llm_credential_verify",
}

#[cfg(test)]
mod tests {
    use super::Op;

    /// Two ops sharing a label put two verbs in one row, which makes both
    /// unreadable and either unbillable.
    #[test]
    fn every_op_has_its_own_label() {
        let mut labels: Vec<&str> = Op::ALL.iter().map(|o| o.as_str()).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "duplicate op label");
    }

    /// The labels are a dimension in somebody's analytics store, so they stay
    /// in one shape: lowercase snake_case, no spaces, no punctuation.
    #[test]
    fn labels_are_snake_case() {
        for op in Op::ALL {
            let label = op.as_str();
            assert!(
                !label.is_empty()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "`{label}` is not a snake_case label"
            );
        }
    }
}
