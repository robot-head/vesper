import ResourceModel42.Syntax
import ResourceModel42.Checker
import ResourceModel42.Evaluator
import ResourceModel42.ErasureProofs
import ResourceModel42.LedgerProofs

/-! The finite executable resource model and its implemented proof components.
Independent acceptance imports this aggregate; the aggregate never imports
acceptance assertions. Remaining checked/operational proof modules are added
only after their genuine compiled implementations are available.
-/
