CREATE TABLE capture_erasure_floor (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    install uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence >= 0),
    tip bytea NOT NULL CHECK (octet_length(tip) = 32)
);
