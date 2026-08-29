import {beforeEach, describe, expect, it} from 'vitest'
import {createEnvironment, getEnvironment, resetFixtureState} from '@/fixture/stores'
import {listTemplateReleases} from '@/fixture/stores/templateReleaseStore'
import {nowIso} from '@/fixture/utils/clock'

describe('fixture environment eligibility', () => {
  beforeEach(() => resetFixtureState())

  it('keeps a newly created environment eligible for a bounded future window', () => {
    const release = listTemplateReleases('course-101')[0]!
    const accepted = createEnvironment({
      courseId: 'course-101', releaseId: release.id, releaseVersion: release.version,
      displayLabel: 'Eligibility regression',
    }, {
      actorId: 'fixture-actor-student', role: 'student', courseIds: ['course-101'],
    }, 'fixture-eligibility-regression')
    const environment = getEnvironment(accepted.environmentId)!

    expect(environment.observedState).toBe('ready')
    expect(Date.parse(environment.eligibilityExpiresAt)).toBeGreaterThan(Date.parse(nowIso()))
  })
})
