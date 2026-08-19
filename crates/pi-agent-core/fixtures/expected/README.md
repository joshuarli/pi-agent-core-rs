# Expected canonical results

This directory contains optional golden results for declarative fixtures. Files use the same
fixture ID and `.json` extension as their source. They must be complete
`canonical_parity_result` objects, not provider or runner event dumps. Assertions embedded in a
declarative fixture remain useful for focused cases that do not need a full golden file.
