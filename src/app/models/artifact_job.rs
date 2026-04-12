use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QuerySelect};

use crate::{
    app::models::{
        hyperlink_processing_job::{self as hyperlink_processing_job_model, ProcessingQueueSender},
        settings::{self, ArtifactCollectionSettings},
    },
    entity::{
        hyperlink::{self, HyperlinkSourceType},
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::HyperlinkProcessingJobKind,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactFetchMode {
    EnsurePresent,
    RefetchTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactDependencyCondition {
    SourceReady,
    OgReady,
    ReadabilityReady,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactDependency {
    Condition(ArtifactDependencyCondition),
}

pub trait JobSpec: Sync {
    fn kind(&self) -> HyperlinkProcessingJobKind;
    fn enabled(&self, settings: ArtifactCollectionSettings) -> bool;
    fn depends_on(&self) -> &'static [ArtifactDependency];
    fn is_satisfied(&self, state: &ArtifactState) -> bool;
}

struct SnapshotJobSpec;
struct OgJobSpec;
struct ReadabilityJobSpec;

const OG_DEPENDENCIES: [ArtifactDependency; 1] = [ArtifactDependency::Condition(
    ArtifactDependencyCondition::SourceReady,
)];
const READABILITY_DEPENDENCIES: [ArtifactDependency; 1] = [ArtifactDependency::Condition(
    ArtifactDependencyCondition::SourceReady,
)];

static SNAPSHOT_JOB_SPEC: SnapshotJobSpec = SnapshotJobSpec;
static OG_JOB_SPEC: OgJobSpec = OgJobSpec;
static READABILITY_JOB_SPEC: ReadabilityJobSpec = ReadabilityJobSpec;

impl JobSpec for SnapshotJobSpec {
    fn kind(&self) -> HyperlinkProcessingJobKind {
        HyperlinkProcessingJobKind::Snapshot
    }

    fn enabled(&self, settings: ArtifactCollectionSettings) -> bool {
        settings.collect_source
    }

    fn depends_on(&self) -> &'static [ArtifactDependency] {
        &[]
    }

    fn is_satisfied(&self, state: &ArtifactState) -> bool {
        state.condition_satisfied(ArtifactDependencyCondition::SourceReady)
    }
}

impl JobSpec for OgJobSpec {
    fn kind(&self) -> HyperlinkProcessingJobKind {
        HyperlinkProcessingJobKind::Og
    }

    fn enabled(&self, settings: ArtifactCollectionSettings) -> bool {
        settings.collect_og
    }

    fn depends_on(&self) -> &'static [ArtifactDependency] {
        &OG_DEPENDENCIES
    }

    fn is_satisfied(&self, state: &ArtifactState) -> bool {
        state.condition_satisfied(ArtifactDependencyCondition::OgReady)
    }
}

impl JobSpec for ReadabilityJobSpec {
    fn kind(&self) -> HyperlinkProcessingJobKind {
        HyperlinkProcessingJobKind::Readability
    }

    fn enabled(&self, settings: ArtifactCollectionSettings) -> bool {
        settings.collect_readability
    }

    fn depends_on(&self) -> &'static [ArtifactDependency] {
        &READABILITY_DEPENDENCIES
    }

    fn is_satisfied(&self, state: &ArtifactState) -> bool {
        state.condition_satisfied(ArtifactDependencyCondition::ReadabilityReady)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArtifactState {
    has_snapshot_warc: bool,
    has_pdf_source: bool,
    has_og_meta: bool,
    has_readable_text: bool,
    has_readable_meta: bool,
}

impl ArtifactState {
    async fn load(connection: &DatabaseConnection, hyperlink_id: i32) -> Result<Self, DbErr> {
        let kinds = hyperlink_artifact::Entity::find()
            .select_only()
            .column(hyperlink_artifact::Column::Kind)
            .filter(hyperlink_artifact::Column::HyperlinkId.eq(hyperlink_id))
            .filter(hyperlink_artifact::Column::Kind.is_in([
                HyperlinkArtifactKind::SnapshotWarc,
                HyperlinkArtifactKind::PdfSource,
                HyperlinkArtifactKind::OgMeta,
                HyperlinkArtifactKind::ReadableText,
                HyperlinkArtifactKind::ReadableHtml,
                HyperlinkArtifactKind::ReadableMeta,
            ]))
            .into_tuple::<HyperlinkArtifactKind>()
            .all(connection)
            .await?;

        let mut state = Self::default();
        for kind in kinds {
            match kind {
                HyperlinkArtifactKind::SnapshotWarc => state.has_snapshot_warc = true,
                HyperlinkArtifactKind::PdfSource => state.has_pdf_source = true,
                HyperlinkArtifactKind::OgMeta => state.has_og_meta = true,
                HyperlinkArtifactKind::ReadableText => state.has_readable_text = true,
                HyperlinkArtifactKind::ReadableHtml => {}
                HyperlinkArtifactKind::ReadableMeta => state.has_readable_meta = true,
                _ => {}
            }
        }

        Ok(state)
    }

    fn condition_satisfied(&self, condition: ArtifactDependencyCondition) -> bool {
        match condition {
            ArtifactDependencyCondition::SourceReady => {
                self.has_snapshot_warc || self.has_pdf_source
            }
            ArtifactDependencyCondition::OgReady => self.has_og_meta,
            ArtifactDependencyCondition::ReadabilityReady => {
                self.has_readable_text && self.has_readable_meta
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtifactJobResolveResult {
    EnqueuedRequested {
        requested_kind: HyperlinkProcessingJobKind,
        queued_job_id: i32,
    },
    EnqueuedDependency {
        requested_kind: HyperlinkProcessingJobKind,
        dependency_kind: HyperlinkProcessingJobKind,
        queued_job_id: i32,
    },
    AlreadySatisfied {
        requested_kind: HyperlinkProcessingJobKind,
    },
    DisabledRequested {
        requested_kind: HyperlinkProcessingJobKind,
    },
    DisabledDependency {
        requested_kind: HyperlinkProcessingJobKind,
        dependency_kind: HyperlinkProcessingJobKind,
    },
    UnfetchableDependency {
        requested_kind: HyperlinkProcessingJobKind,
        dependency_kind: HyperlinkProcessingJobKind,
    },
    UnsupportedArtifactKind {
        artifact_kind: HyperlinkArtifactKind,
    },
    UnsupportedJobKind {
        requested_kind: HyperlinkProcessingJobKind,
    },
}

impl ArtifactJobResolveResult {
    pub fn was_enqueued(self) -> bool {
        matches!(
            self,
            Self::EnqueuedRequested { .. } | Self::EnqueuedDependency { .. }
        )
    }

    pub fn enqueued_kind(self) -> Option<HyperlinkProcessingJobKind> {
        match self {
            Self::EnqueuedRequested { requested_kind, .. } => Some(requested_kind),
            Self::EnqueuedDependency {
                dependency_kind, ..
            } => Some(dependency_kind),
            Self::AlreadySatisfied { .. }
            | Self::DisabledRequested { .. }
            | Self::DisabledDependency { .. }
            | Self::UnfetchableDependency { .. }
            | Self::UnsupportedArtifactKind { .. }
            | Self::UnsupportedJobKind { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DependencyResolution {
    Ready,
    Queue(HyperlinkProcessingJobKind),
    Disabled(HyperlinkProcessingJobKind),
    Unfetchable(HyperlinkProcessingJobKind),
}

pub fn job_kind_for_artifact_kind(
    kind: HyperlinkArtifactKind,
) -> Option<HyperlinkProcessingJobKind> {
    match kind {
        HyperlinkArtifactKind::SnapshotWarc
        | HyperlinkArtifactKind::PdfSource
        | HyperlinkArtifactKind::SnapshotError
        | HyperlinkArtifactKind::ScreenshotWebp
        | HyperlinkArtifactKind::ScreenshotThumbWebp
        | HyperlinkArtifactKind::ScreenshotDarkWebp
        | HyperlinkArtifactKind::ScreenshotThumbDarkWebp
        | HyperlinkArtifactKind::ScreenshotError => Some(HyperlinkProcessingJobKind::Snapshot),
        HyperlinkArtifactKind::OgMeta
        | HyperlinkArtifactKind::OgImage
        | HyperlinkArtifactKind::OgError => Some(HyperlinkProcessingJobKind::Og),
        HyperlinkArtifactKind::ReadableText
        | HyperlinkArtifactKind::ReadableHtml
        | HyperlinkArtifactKind::ReadableMeta
        | HyperlinkArtifactKind::ReadableError => Some(HyperlinkProcessingJobKind::Readability),
        HyperlinkArtifactKind::PaperlessMetadata
        | HyperlinkArtifactKind::OembedMeta
        | HyperlinkArtifactKind::OembedError => None,
    }
}

pub fn artifact_kind_fetch_enabled(
    kind: &HyperlinkArtifactKind,
    settings: ArtifactCollectionSettings,
) -> bool {
    let Some(job_kind) = job_kind_for_artifact_kind(kind.clone()) else {
        return false;
    };
    job_kind_fetch_enabled(job_kind, settings)
}

pub fn job_kind_fetch_enabled(
    kind: HyperlinkProcessingJobKind,
    settings: ArtifactCollectionSettings,
) -> bool {
    job_spec_for_kind(kind).is_some_and(|spec| spec.enabled(settings))
}

pub async fn resolve_and_enqueue_for_artifact_kind(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    artifact_kind: HyperlinkArtifactKind,
    mode: ArtifactFetchMode,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<ArtifactJobResolveResult, DbErr> {
    let settings = settings::load(connection).await?;
    resolve_and_enqueue_for_artifact_kind_with_settings(
        connection,
        hyperlink_id,
        artifact_kind,
        mode,
        settings,
        processing_queue,
    )
    .await
}

pub async fn resolve_and_enqueue_for_artifact_kind_with_settings(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    artifact_kind: HyperlinkArtifactKind,
    mode: ArtifactFetchMode,
    settings: ArtifactCollectionSettings,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<ArtifactJobResolveResult, DbErr> {
    let Some(requested_kind) = job_kind_for_artifact_kind(artifact_kind.clone()) else {
        return Ok(ArtifactJobResolveResult::UnsupportedArtifactKind { artifact_kind });
    };

    resolve_and_enqueue_for_job_kind_with_settings(
        connection,
        hyperlink_id,
        requested_kind,
        mode,
        settings,
        processing_queue,
    )
    .await
}

pub async fn resolve_and_enqueue_for_job_kind(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    requested_kind: HyperlinkProcessingJobKind,
    mode: ArtifactFetchMode,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<ArtifactJobResolveResult, DbErr> {
    let settings = settings::load(connection).await?;
    resolve_and_enqueue_for_job_kind_with_settings(
        connection,
        hyperlink_id,
        requested_kind,
        mode,
        settings,
        processing_queue,
    )
    .await
}

pub async fn resolve_and_enqueue_for_job_kind_with_settings(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    requested_kind: HyperlinkProcessingJobKind,
    mode: ArtifactFetchMode,
    settings: ArtifactCollectionSettings,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<ArtifactJobResolveResult, DbErr> {
    let Some(spec) = job_spec_for_kind(requested_kind.clone()) else {
        return Ok(ArtifactJobResolveResult::UnsupportedJobKind { requested_kind });
    };

    if !spec.enabled(settings) {
        return Ok(ArtifactJobResolveResult::DisabledRequested { requested_kind });
    }

    let hyperlink = hyperlink::Entity::find_by_id(hyperlink_id)
        .one(connection)
        .await?
        .ok_or_else(|| DbErr::Custom(format!("hyperlink {hyperlink_id} not found").into()))?;
    let state = ArtifactState::load(connection, hyperlink_id).await?;
    if requested_kind == HyperlinkProcessingJobKind::Snapshot
        && !state.condition_satisfied(ArtifactDependencyCondition::SourceReady)
        && !source_dependency_is_fetchable(&hyperlink)
    {
        return Ok(ArtifactJobResolveResult::UnfetchableDependency {
            requested_kind,
            dependency_kind: HyperlinkProcessingJobKind::Snapshot,
        });
    }
    let mut visiting = vec![requested_kind.clone()];
    let dependency = resolve_dependencies(spec, &hyperlink, &state, settings, &mut visiting)?;
    match dependency {
        DependencyResolution::Disabled(dependency_kind) => {
            return Ok(ArtifactJobResolveResult::DisabledDependency {
                requested_kind,
                dependency_kind,
            });
        }
        DependencyResolution::Unfetchable(dependency_kind) => {
            return Ok(ArtifactJobResolveResult::UnfetchableDependency {
                requested_kind,
                dependency_kind,
            });
        }
        DependencyResolution::Queue(dependency_kind) => {
            let queued = hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
                connection,
                hyperlink_id,
                dependency_kind.clone(),
                processing_queue,
            )
            .await?;
            return Ok(ArtifactJobResolveResult::EnqueuedDependency {
                requested_kind,
                dependency_kind,
                queued_job_id: queued.id,
            });
        }
        DependencyResolution::Ready => {}
    }

    if matches!(mode, ArtifactFetchMode::EnsurePresent) && spec.is_satisfied(&state) {
        return Ok(ArtifactJobResolveResult::AlreadySatisfied { requested_kind });
    }

    let queued = hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
        connection,
        hyperlink_id,
        requested_kind.clone(),
        processing_queue,
    )
    .await?;
    Ok(ArtifactJobResolveResult::EnqueuedRequested {
        requested_kind,
        queued_job_id: queued.id,
    })
}

fn resolve_dependencies(
    spec: &'static dyn JobSpec,
    hyperlink: &hyperlink::Model,
    state: &ArtifactState,
    settings: ArtifactCollectionSettings,
    visiting: &mut Vec<HyperlinkProcessingJobKind>,
) -> Result<DependencyResolution, DbErr> {
    for dependency in spec.depends_on() {
        let ArtifactDependency::Condition(condition) = dependency;
        if state.condition_satisfied(*condition) {
            continue;
        }

        let dependency_spec = match job_spec_for_condition(*condition) {
            Some(spec) => spec,
            None => {
                return Err(DbErr::Custom(
                    format!("unknown dependency condition: {condition:?}").into(),
                ));
            }
        };
        let dependency_kind = dependency_spec.kind();
        if !dependency_spec.enabled(settings) {
            return Ok(DependencyResolution::Disabled(dependency_kind));
        }
        if dependency_kind == HyperlinkProcessingJobKind::Snapshot
            && !source_dependency_is_fetchable(hyperlink)
        {
            return Ok(DependencyResolution::Unfetchable(dependency_kind));
        }

        if visiting.contains(&dependency_kind) {
            let chain = visiting
                .iter()
                .map(|kind| format!("{kind:?}"))
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(DbErr::Custom(
                format!("artifact job dependency cycle detected: {chain} -> {dependency_kind:?}")
                    .into(),
            ));
        }

        visiting.push(dependency_kind.clone());
        let resolution =
            resolve_dependencies(dependency_spec, hyperlink, state, settings, visiting)?;
        visiting.pop();

        match resolution {
            DependencyResolution::Ready => {}
            DependencyResolution::Queue(kind) => return Ok(DependencyResolution::Queue(kind)),
            DependencyResolution::Disabled(kind) => {
                return Ok(DependencyResolution::Disabled(kind));
            }
            DependencyResolution::Unfetchable(kind) => {
                return Ok(DependencyResolution::Unfetchable(kind));
            }
        }

        if !dependency_spec.is_satisfied(&state) {
            return Ok(DependencyResolution::Queue(dependency_kind));
        }
    }

    Ok(DependencyResolution::Ready)
}

fn job_spec_for_kind(kind: HyperlinkProcessingJobKind) -> Option<&'static dyn JobSpec> {
    match kind {
        HyperlinkProcessingJobKind::Snapshot => Some(&SNAPSHOT_JOB_SPEC),
        HyperlinkProcessingJobKind::Og => Some(&OG_JOB_SPEC),
        HyperlinkProcessingJobKind::Readability => Some(&READABILITY_JOB_SPEC),
        HyperlinkProcessingJobKind::Oembed | HyperlinkProcessingJobKind::SublinkDiscovery => None,
    }
}

fn job_spec_for_condition(condition: ArtifactDependencyCondition) -> Option<&'static dyn JobSpec> {
    match condition {
        ArtifactDependencyCondition::SourceReady => Some(&SNAPSHOT_JOB_SPEC),
        ArtifactDependencyCondition::OgReady => Some(&OG_JOB_SPEC),
        ArtifactDependencyCondition::ReadabilityReady => Some(&READABILITY_JOB_SPEC),
    }
}

fn source_dependency_is_fetchable(hyperlink: &hyperlink::Model) -> bool {
    match hyperlink.source_type {
        HyperlinkSourceType::Pdf | HyperlinkSourceType::Html | HyperlinkSourceType::Unknown => {
            is_absolute_http_or_https_url(hyperlink.url.as_str())
        }
    }
}

fn is_absolute_http_or_https_url(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}
#[cfg(test)]
#[path = "../../../tests/unit/app_models_artifact_job.rs"]
mod tests;
