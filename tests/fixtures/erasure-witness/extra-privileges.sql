GRANT gateway_witness_schema TO gateway_witness_inherited;
GRANT TRIGGER ON erasure_witness.position TO gateway_witness_trigger;
GRANT UPDATE (tip) ON erasure_witness.position TO gateway_witness_column;
GRANT MAINTAIN ON erasure_witness.position TO gateway_witness_maintain;
GRANT REFERENCES (install) ON erasure_witness.position TO gateway_witness_reference;
GRANT CREATE ON SCHEMA erasure_witness TO gateway_witness_create;
