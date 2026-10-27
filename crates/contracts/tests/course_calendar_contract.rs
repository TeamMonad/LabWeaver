//! Course calendar input normalization and generated invitation request invariants.
use contracts::{
    CourseId, Revision,
    membership_governance::{CourseCalendar, CourseCalendarState, UpdateCourseCalendar},
    schema::generate_all,
};
use serde_json::{Value, json};

#[test]
fn invitation_request_bodies_and_calendar_patch_survive_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let artifacts = generate_all()?;
    let public: Value = serde_json::from_slice(
        &artifacts
            .iter()
            .find(|a| a.relative_path == "schemas/openapi/labweaver-public.v1.json")
            .ok_or("missing public OpenAPI")?
            .bytes,
    )?;
    for (path, name) in [
        (
            "/api/v1/projects/{projectId}/member-invitations",
            "create-project-membership-invitation",
        ),
        (
            "/api/v1/courses/{courseId}/member-invitations",
            "create-course-membership-invitation",
        ),
    ] {
        let operation = &public["paths"][path]["post"];
        assert_eq!(operation["requestBody"]["required"], true, "{path}");
        assert_eq!(
            operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            format!("../contracts/v1/http/{name}.schema.json"),
            "{path}"
        );
    }
    let calendar = &public["paths"]["/api/v1/courses/{courseId}/calendar"];
    assert!(calendar.get("put").is_none());
    assert_eq!(calendar["patch"]["operationId"], "updateCourseCalendar");
    assert!(
        calendar["patch"]["x-labweaver-errors"]
            .as_array()
            .ok_or("calendar errors missing")?
            .iter()
            .any(|code| code == "LW_COURSE_CALENDAR_CONFLICT")
    );
    Ok(())
}

#[test]
fn offset_calendar_input_resolves_utc_and_exact_open_due_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let input: UpdateCourseCalendar = serde_json::from_value(json!({
        "openAt": "2026-10-01T11:00:00.000+02:00",
        "dueAt": "2026-10-01T12:00:00.000+02:00",
        "timeZone": "Europe/Berlin",
        "latePolicy": "allow_with_mark",
        "lateUntil": "2026-10-01T13:00:00.000+02:00"
    }))?;
    assert_eq!(
        input.open_at.ok_or("open missing")?.to_string(),
        "2026-10-01T09:00:00.000Z"
    );
    assert_eq!(input.due_at.to_string(), "2026-10-01T10:00:00.000Z");
    assert_eq!(
        input.late_until.ok_or("late missing")?.to_string(),
        "2026-10-01T11:00:00.000Z"
    );
    let calendar = CourseCalendar {
        course_id: CourseId::new(),
        revision: Revision::new(1)?,
        open_at: input.open_at,
        due_at: input.due_at,
        time_zone: input.time_zone,
        state: CourseCalendarState::NotOpen,
        late_policy: input.late_policy,
        late_until: input.late_until,
    };
    assert_eq!(
        calendar.state_at("2026-10-01T08:59:59.999Z".parse()?),
        CourseCalendarState::NotOpen
    );
    assert_eq!(
        calendar.state_at("2026-10-01T09:00:00.000Z".parse()?),
        CourseCalendarState::Open
    );
    assert_eq!(
        calendar.state_at("2026-10-01T10:00:00.000Z".parse()?),
        CourseCalendarState::Late
    );
    assert_eq!(
        calendar.state_at("2026-10-01T11:00:00.000Z".parse()?),
        CourseCalendarState::Closed
    );
    Ok(())
}
