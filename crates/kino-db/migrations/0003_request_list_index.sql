CREATE INDEX requests_created_at_id_idx
ON requests (created_at, id);

CREATE INDEX requests_state_created_at_id_idx
ON requests (state, created_at, id);
