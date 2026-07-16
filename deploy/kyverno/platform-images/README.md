# Platform image admission tests

The authoritative policy is rendered by `deploy/helm/labweaver` from the
versioned Private Sigstore trust inputs. `tests/kyverno-test.yaml` uses only
fictional public-safe identities and proves the registry/digest validation
rule. Connected verification of the real Fulcio, CT and Rekor identity must run
on the controlled Linux router and is never inferred from this fixture.
