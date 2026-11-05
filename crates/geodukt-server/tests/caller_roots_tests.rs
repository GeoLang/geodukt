use std::collections::HashMap;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::geometry::{FeatureGeometry, Point};
use geodukt_io::shapefile_io::write_shapefile;
use geodukt_server::RunStore;
use geodukt_server::auth::AuthConfig;
use geodukt_server::caller_roots::{CallerRoots, caller_directory_name};
use geodukt_server::create_router_with_store;
use tower::ServiceExt;

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const CALLER: &str = "user-a";
const OTHER_CALLER: &str = "user-b";
const TOKEN_LIFETIME_SECONDS: u64 = 60;
const EDITOR_ROLE: &str = "editor";
const POINT_COLLECTION: &str = r#"{"type":"FeatureCollection","features":[
    {"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;

struct Volume {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}

impl Volume {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("outputs");
        for subject in [CALLER, OTHER_CALLER] {
            let directory = root.join(caller_directory_name(subject));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("in.geojson"), POINT_COLLECTION).unwrap();
        }
        Self {
            _temporary: temporary,
            root,
        }
    }

    fn directory_of(&self, subject: &str) -> PathBuf {
        self.root.join(caller_directory_name(subject))
    }

    fn router(&self, auth: AuthConfig) -> axum::Router {
        create_router_with_store(
            auth,
            RunStore::open(None).unwrap(),
            CallerRoots::new(std::slice::from_ref(&self.root)).unwrap(),
        )
    }
}

fn token_for(subject: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &serde_json::json!({
            "sub": subject,
            "exp": now + TOKEN_LIFETIME_SECONDS,
            "role": EDITOR_ROLE,
        }),
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}

fn manifest(source: &Path, sink: &Path) -> String {
    format!(
        r#"
[project]
name = "confined"

[[source]]
name = "points"
format = "geojson"
path = "{source}"

[[sink]]
name = "written"
input = "points"
format = "geojson"
path = "{sink}"
"#,
        source = source.display(),
        sink = sink.display()
    )
}

async fn send(app: axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn run_as(app: axum::Router, subject: Option<&str>, manifest: &str) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json");
    if let Some(subject) = subject {
        builder = builder.header("authorization", format!("Bearer {}", token_for(subject)));
    }
    let request = builder
        .body(Body::from(
            serde_json::json!({"manifest": manifest}).to_string(),
        ))
        .unwrap();
    send(app, request).await
}

async fn recorded_run_count(app: axum::Router, subject: &str) -> usize {
    let request = Request::builder()
        .uri("/runs")
        .header("authorization", format!("Bearer {}", token_for(subject)))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str::<Vec<serde_json::Value>>(&body)
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_manifest_inside_the_callers_own_directory_runs() {
    let volume = Volume::new();
    let own = volume.directory_of(CALLER);
    let sink = own.join("new").join("out.geojson");

    let (status, body) = run_as(
        volume.router(AuthConfig::new(Some(SECRET.into()))),
        Some(CALLER),
        &manifest(&own.join("in.geojson"), &sink),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(sink.exists());
}

#[tokio::test]
async fn another_callers_source_is_refused_before_anything_runs() {
    let volume = Volume::new();
    let app = volume.router(AuthConfig::new(Some(SECRET.into())));
    let own = volume.directory_of(CALLER);
    let sink = own.join("out.geojson");
    let theirs = volume.directory_of(OTHER_CALLER).join("in.geojson");

    let (status, body) = run_as(app.clone(), Some(CALLER), &manifest(&theirs, &sink)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("source 'points'"), "{body}");
    assert!(!sink.exists(), "the pipeline ran");
    assert_eq!(recorded_run_count(app, CALLER).await, 0);
}

#[tokio::test]
async fn a_sink_outside_the_callers_directory_is_not_written() {
    let volume = Volume::new();
    let own = volume.directory_of(CALLER);
    let theirs = volume.directory_of(OTHER_CALLER);
    let other_directory_name = caller_directory_name(OTHER_CALLER);
    let escapes = [
        theirs.join("out.geojson"),
        own.join("..")
            .join(&other_directory_name)
            .join("out.geojson"),
        own.join("missing")
            .join("..")
            .join("..")
            .join(&other_directory_name)
            .join("out.geojson"),
        volume.root.join("out.geojson"),
    ];

    for sink in escapes {
        let (status, body) = run_as(
            volume.router(AuthConfig::new(Some(SECRET.into()))),
            Some(CALLER),
            &manifest(&own.join("in.geojson"), &sink),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{}: {body}", sink.display());
        assert!(body.contains("sink 'written'"), "{body}");
        assert!(!theirs.join("out.geojson").exists(), "{}", sink.display());
        assert!(
            !volume.root.join("out.geojson").exists(),
            "{}",
            sink.display()
        );
    }
}

#[tokio::test]
async fn a_symlink_pointing_out_of_the_callers_directory_is_refused() {
    let volume = Volume::new();
    let own = volume.directory_of(CALLER);
    let theirs = volume.directory_of(OTHER_CALLER);
    symlink(theirs.join("in.geojson"), own.join("linked.geojson")).unwrap();
    symlink(&theirs, own.join("linked_directory")).unwrap();
    symlink(theirs.join("planted.geojson"), own.join("dangling.geojson")).unwrap();

    let cases = [
        (
            own.join("linked.geojson"),
            own.join("out.geojson"),
            "source",
        ),
        (
            own.join("in.geojson"),
            own.join("linked_directory").join("planted.geojson"),
            "sink",
        ),
        (own.join("in.geojson"), own.join("dangling.geojson"), "sink"),
    ];
    for (source, sink, refused_kind) in cases {
        let (status, body) = run_as(
            volume.router(AuthConfig::new(Some(SECRET.into()))),
            Some(CALLER),
            &manifest(&source, &sink),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(body.starts_with(refused_kind), "{body}");
        assert!(!theirs.join("planted.geojson").exists());
        assert!(!own.join("out.geojson").exists());
    }
}

fn write_point_shapefile(path: &Path, name: &str) {
    let feature = Feature {
        geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
        properties: HashMap::from([("name".to_string(), Value::String(name.to_string()))]),
    };
    write_shapefile(path, &FeatureCollection::new(vec![feature], None)).unwrap();
}

fn shapefile_manifest(source: &Path, sink: &Path) -> String {
    manifest(source, sink).replacen(r#"format = "geojson""#, r#"format = "shapefile""#, 1)
}

#[tokio::test]
async fn a_shapefile_with_regular_sidecars_runs() {
    let volume = Volume::new();
    let own = volume.directory_of(CALLER);
    let source = own.join("in.shp");
    let sink = own.join("out.geojson");
    write_point_shapefile(&source, "from-own-dbf");

    let (status, body) = run_as(
        volume.router(AuthConfig::new(Some(SECRET.into()))),
        Some(CALLER),
        &shapefile_manifest(&source, &sink),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let written = std::fs::read_to_string(&sink).unwrap();
    assert!(written.contains("from-own-dbf"), "{written}");
}

#[tokio::test]
async fn a_shapefile_sidecar_symlinked_out_of_the_callers_directory_is_refused() {
    let volume = Volume::new();
    let own = volume.directory_of(CALLER);
    let theirs = volume.directory_of(OTHER_CALLER).join("in.shp");
    write_point_shapefile(&theirs, "from-their-dbf");

    for extension in ["shx", "dbf"] {
        let source = own.join(format!("linked_{extension}.shp"));
        let sink = own.join(format!("linked_{extension}.geojson"));
        write_point_shapefile(&source, "from-own-dbf");
        let sidecar = source.with_extension(extension);
        std::fs::remove_file(&sidecar).unwrap();
        symlink(theirs.with_extension(extension), &sidecar).unwrap();

        let (status, body) = run_as(
            volume.router(AuthConfig::new(Some(SECRET.into()))),
            Some(CALLER),
            &shapefile_manifest(&source, &sink),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("without following symlinks"), "{body}");
        assert!(!body.contains("from-their-dbf"), "{body}");
        assert!(!sink.exists(), "{extension}");
    }
}

#[tokio::test]
async fn a_shapefile_sink_sidecar_symlinked_out_of_the_callers_directory_is_not_written() {
    const OUTSIDE_CONTENT: &str = "belongs to the other caller";
    let volume = Volume::new();
    let own = volume.directory_of(CALLER);
    let theirs = volume.directory_of(OTHER_CALLER);
    let source = own.join("named.geojson");
    std::fs::write(
        &source,
        r#"{"type":"FeatureCollection","features":[
    {"type":"Feature","properties":{"name":"park"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
    )
    .unwrap();

    for extension in ["shx", "dbf", "prj"] {
        let existing_target = theirs.join(format!("existing.{extension}"));
        let dangling_target = theirs.join(format!("planted.{extension}"));
        std::fs::write(&existing_target, OUTSIDE_CONTENT).unwrap();

        for (sink_stem, target) in [
            ("existing", &existing_target),
            ("dangling", &dangling_target),
        ] {
            let sink = own.join(format!("{sink_stem}_{extension}.shp"));
            symlink(target, sink.with_extension(extension)).unwrap();
            let sink_manifest = manifest(&source, &sink).replace(
                "input = \"points\"\nformat = \"geojson\"",
                "input = \"points\"\nformat = \"shapefile\"",
            );

            let (status, body) = run_as(
                volume.router(AuthConfig::new(Some(SECRET.into()))),
                Some(CALLER),
                &sink_manifest,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert!(body.contains("without following symlinks"), "{body}");
        }
        assert_eq!(
            std::fs::read_to_string(&existing_target).unwrap(),
            OUTSIDE_CONTENT
        );
        assert!(!dangling_target.exists(), "{extension}");
    }
}

#[tokio::test]
async fn a_request_without_a_verified_caller_is_refused() {
    let volume = Volume::new();
    let own = volume.directory_of(CALLER);
    let sink = own.join("out.geojson");

    let (status, body) = run_as(
        volume.router(AuthConfig::new(None)),
        None,
        &manifest(&own.join("in.geojson"), &sink),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(!sink.exists());
}

#[test]
fn a_missing_caller_root_stops_the_server_from_starting() {
    let temporary = tempfile::tempdir().unwrap();
    let error = CallerRoots::new(&[temporary.path().join("absent")]).unwrap_err();
    assert!(error.to_string().contains("absent"), "{error}");
}
