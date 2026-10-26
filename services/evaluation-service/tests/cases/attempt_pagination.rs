//! Evaluation's real attempt lifecycle remains stable across concurrent page reads.
use super::*;
use contracts::http::TaskProgressQuery;
use evaluation_service::task_progress::EvaluationTaskProgressError;

#[tokio::test]
async fn run_attempt_cursor_survives_insert_delete_and_rejects_other_scopes()
-> Result<(), Box<dyn std::error::Error>> {
    pagination_case(false).await
}

#[tokio::test]
async fn submission_attempt_cursor_survives_insert_delete_and_rejects_other_scopes()
-> Result<(), Box<dyn std::error::Error>> {
    pagination_case(true).await
}

#[allow(clippy::too_many_lines)]
async fn pagination_case(submission: bool) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(single_score_spec()?).await?;
    let run = fixture.create_seeded_run("cursor-history").await?;
    let mut lease = fixture
        .store
        .claim_next_step("cursor-worker", Duration::from_secs(30))
        .await?
        .ok_or("missing attempt")?;
    for number in 1..=3 {
        lease = retry(&fixture, run.id, lease, number).await?;
    }
    let query = TaskProgressQuery {
        cursor: None,
        limit: Some(1),
    };
    let first = read(&fixture, &run, submission, &query).await?;
    assert_eq!(first.history[0].progress.attempt, 3);
    let continuation = TaskProgressQuery {
        cursor: first.next_cursor.clone(),
        ..query.clone()
    };
    assert!(continuation.cursor.is_some());
    for foreign_course in [Some(CourseId::new()), None] {
        set_course(&fixture.pool, run.id, foreign_course).await?;
        assert_http_400(read(&fixture, &run, submission, &continuation).await)?;
    }
    let without_course = read(&fixture, &run, submission, &query).await?;
    let no_course_query = TaskProgressQuery {
        cursor: without_course.next_cursor,
        ..query.clone()
    };
    assert!(no_course_query.cursor.is_some());
    set_course(&fixture.pool, run.id, run.course_id).await?;
    assert_http_400(read(&fixture, &run, submission, &no_course_query).await)?;
    // An owner can use only the cursor's original run/submission endpoint.
    assert_http_400(read(&fixture, &run, !submission, &continuation).await)?;
    assert_http_400(
        fixture
            .store
            .task_progress(
                ProjectId::new(),
                fixture.actor_id,
                run.id,
                None,
                &continuation,
            )
            .await,
    )?;
    assert_http_400(
        fixture
            .store
            .task_progress(
                fixture.project_id,
                ActorId::new(),
                run.id,
                None,
                &continuation,
            )
            .await,
    )?;
    let mut forged = first.next_cursor.clone().ok_or("missing cursor")?;
    let last = forged.pop().ok_or("empty cursor")?;
    forged.push(if last == 'A' { 'B' } else { 'A' });
    assert_http_400(
        read(
            &fixture,
            &run,
            submission,
            &TaskProgressQuery {
                cursor: Some(forged),
                ..query.clone()
            },
        )
        .await,
    )?;
    let other_submission = FrozenSubmissionId::new();
    fixture
        .seed_frozen_submission_for(
            other_submission,
            fixture.project_id,
            Some(fixture.course_id),
            fixture.actor_id,
        )
        .await?;
    let release = fixture.store.load_release(run.release_id).await?;
    let mut request = fixture.create_run_request(&release, "cursor-other-run")?;
    request.frozen_submission_id = other_submission;
    let other = match fixture
        .store
        .create_run(
            &request,
            &idempotency("cursor-other-run")?,
            fixture.now().await?,
            "cursor-other-run",
            &admission(&release),
        )
        .await?
    {
        EvaluationRunReservation::Created(run) | EvaluationRunReservation::Replayed(run) => run,
    };
    assert_http_400(read(&fixture, &other, submission, &continuation).await)?;
    fixture
        .store
        .request_cancellation(
            other.id,
            &mutation(
                fixture.project_id,
                fixture.course_id,
                other.revision,
                fixture.actor_id,
            ),
            &idempotency("cursor-cancel-other")?,
            fixture.now().await?,
            "cursor-cancel-other",
        )
        .await?;
    // The writer completes another real retry and deletes the old page anchor.
    let _new_lease = retry(&fixture, run.id, lease, 4).await?;
    let mut writer = fixture.pool.acquire().await?;
    sqlx::query(
        "DELETE FROM evaluation.evaluation_step_attempts WHERE step_run_id=$1 AND attempt=3",
    )
    .bind(first.history[0].step_run_id.as_uuid())
    .execute(&mut *writer)
    .await?;
    let mut next = continuation.clone();
    let mut seen = Vec::new();
    loop {
        let page = read(&fixture, &run, submission, &next).await?;
        assert_eq!(page.current[0].progress.attempt, 5);
        seen.extend(page.history.iter().map(|entry| entry.progress.attempt));
        match page.next_cursor {
            Some(value) => next.cursor = Some(value),
            None => break,
        }
    }
    assert_eq!(seen, vec![2, 1]);
    let rotated = fixture
        .store
        .clone()
        .with_cursor_signing_keys(vec![8; 32], Some(vec![7; 32]));
    let rotated_page = if submission {
        rotated
            .task_progress_for_submission(
                fixture.project_id,
                fixture.actor_id,
                run.frozen_submission_id,
                None,
                &continuation,
            )
            .await?
    } else {
        rotated
            .task_progress(
                fixture.project_id,
                fixture.actor_id,
                run.id,
                None,
                &continuation,
            )
            .await?
    };
    assert_eq!(rotated_page.history[0].progress.attempt, 2);
    // Ownership is checked afresh, independent of the signed boundary.
    sqlx::query("UPDATE evaluation.evaluation_runs SET actor_id=$2 WHERE run_id=$1")
        .bind(run.id.as_uuid())
        .bind(ActorId::new().as_uuid())
        .execute(&fixture.pool)
        .await?;
    assert!(matches!(
        read(&fixture, &run, submission, &continuation).await,
        Err(EvaluationTaskProgressError::Store(
            EvaluationControlStoreError::RunNotFound
        ))
    ));
    Ok(())
}

async fn read(
    fixture: &TestContext,
    run: &contracts::evaluation::EvaluationRun,
    submission: bool,
    query: &TaskProgressQuery,
) -> Result<contracts::evaluation::EvaluationRunTaskProgress, EvaluationTaskProgressError> {
    if submission {
        fixture
            .store
            .task_progress_for_submission(
                fixture.project_id,
                fixture.actor_id,
                run.frozen_submission_id,
                None,
                query,
            )
            .await
    } else {
        fixture
            .store
            .task_progress(fixture.project_id, fixture.actor_id, run.id, None, query)
            .await
    }
}

fn assert_http_400<T>(
    result: Result<T, EvaluationTaskProgressError>,
) -> Result<(), Box<dyn std::error::Error>> {
    use axum::response::IntoResponse as _;
    let error = result.err().ok_or("cursor was accepted")?;
    assert!(matches!(error, EvaluationTaskProgressError::PageInvalid));
    let response = evaluation_service::api::EvaluationApiError::TaskProgress(error).into_response();
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    Ok(())
}

async fn retry(
    fixture: &TestContext,
    run_id: EvaluationRunId,
    lease: evaluation_service::EvaluationStepLease,
    number: u32,
) -> Result<evaluation_service::EvaluationStepLease, Box<dyn std::error::Error>> {
    let failed = complete_leased_step(
        fixture,
        run_id,
        &lease,
        &failed_completion(true),
        "cursor-history",
    )
    .await?;
    fixture
        .store
        .retry_step(
            run_id,
            lease.step_run_id,
            &mutation(
                fixture.project_id,
                fixture.course_id,
                failed.revision,
                fixture.actor_id,
            ),
            &idempotency(&format!("cursor-retry-{number}"))?,
            fixture.now().await?,
            "cursor-history",
        )
        .await?;
    Ok(fixture
        .store
        .claim_next_step("cursor-worker", Duration::from_secs(30))
        .await?
        .ok_or("missing retry")?)
}

async fn set_course(
    pool: &sqlx::PgPool,
    run_id: EvaluationRunId,
    course: Option<CourseId>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE evaluation.evaluation_runs SET course_id=$2,contract=jsonb_set(contract,'{courseId}',COALESCE(to_jsonb($2::uuid),'null'::jsonb)) WHERE run_id=$1")
        .bind(run_id.as_uuid()).bind(course.map(CourseId::as_uuid)).execute(pool).await?;
    Ok(())
}
