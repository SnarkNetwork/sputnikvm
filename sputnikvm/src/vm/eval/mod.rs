use utils::bigint::M256;
use utils::gas::Gas;
use super::commit::{AccountState, BlockhashState};
use super::errors::{RequireError, MachineError, CommitError, EvalError, PCError};
use super::{Stack, Context, BlockHeader, Patch, PC, Storage, Memory, AccountCommitment};

use self::check::check_opcode;
use self::run::run_opcode;
use self::cost::{gas_refund, gas_stipend, gas_cost, memory_cost};

mod cost;
mod run;
mod check;

/// A VM state without PC.
pub struct State<M, S> {
    pub memory: M,
    pub stack: Stack,

    pub context: Context,
    pub block: BlockHeader,
    pub patch: Patch,

    pub out: Vec<u8>,

    pub memory_gas: Gas,
    pub used_gas: Gas,
    pub refunded_gas: Gas,

    pub account_state: AccountState<S>,
    pub blockhash_state: BlockhashState,
}

/// A VM state with PC.
pub struct Machine<M, S> {
    state: State<M, S>,
    pc: PC,
    status: MachineStatus,
}

#[derive(Debug, Clone)]
pub enum MachineStatus {
    Running,
    ExitedOk,
    ExitedErr(MachineError),
    InvokeCall(Context, (M256, M256)),
}

#[derive(Debug, Clone)]
pub enum ControlCheck {
    Jump(usize),
}

#[derive(Debug, Clone)]
pub enum Control {
    Jump(usize),
    InvokeCall(Context, (M256, M256)),
}

impl<M: Memory + Default, S: Storage + Default + Clone> Machine<M, S> {
    pub fn new(context: Context, block: BlockHeader, patch: Patch) -> Self {
        Machine {
            pc: PC::new(context.code.as_slice()),
            status: MachineStatus::Running,
            state: State {
                memory: M::default(),
                stack: Stack::default(),

                context: context,
                block: block,
                patch: patch,

                out: Vec::new(),

                memory_gas: Gas::zero(),
                used_gas: Gas::zero(),
                refunded_gas: Gas::zero(),

                account_state: AccountState::default(),
                blockhash_state: BlockhashState::default(),
            },
        }
    }

    pub fn derive(&self, context: Context) -> Self {
        Machine {
            pc: PC::new(context.code.as_slice()),
            status: MachineStatus::Running,
            state: State {
                memory: M::default(),
                stack: Stack::default(),

                context: context,
                block: self.state.block.clone(),
                patch: self.state.patch.clone(),

                out: Vec::new(),

                memory_gas: Gas::zero(),
                used_gas: Gas::zero(),
                refunded_gas: Gas::zero(),

                account_state: self.state.account_state.clone(),
                blockhash_state: self.state.blockhash_state.clone()
            },
        }
    }

    pub fn commit_account(&mut self, commitment: AccountCommitment<S>) -> Result<(), CommitError> {
        self.state.account_state.commit(commitment)
    }

    pub fn commit_blockhash(&mut self, number: M256, hash: M256) -> Result<(), CommitError> {
        self.state.blockhash_state.commit(number, hash)
    }

    #[allow(unused_variables)]
    pub fn apply_sub(&mut self, sub: Machine<M, S>) {
        unimplemented!()
    }

    pub fn check(&self) -> Result<(), EvalError> {
        let instruction = self.pc.peek()?;
        check_opcode(instruction, &self.state).and_then(|v| {
            match v {
                None => Ok(()),
                Some(ControlCheck::Jump(dest)) => {
                    if self.pc.is_valid(dest) {
                        Ok(())
                    } else {
                        Err(EvalError::Machine(MachineError::PC(PCError::BadJumpDest)))
                    }
                }
            }
        })
    }

    pub fn step(&mut self) -> Result<(), RequireError> {
        match self.check() {
            Ok(()) => (),
            Err(EvalError::Machine(error)) => {
                self.status = MachineStatus::ExitedErr(error);
                return Ok(());
            },
            Err(EvalError::Require(error)) => {
                return Err(error);
            },
        };

        let instruction = self.pc.peek().unwrap();
        let memory_cost = memory_cost(instruction, &self.state);
        let gas_cost = gas_cost(instruction, &self.state);
        let gas_stipend = gas_stipend(instruction, &self.state);
        let gas_refund = gas_refund(instruction, &self.state);

        if self.state.context.gas_limit < memory_cost + gas_cost {
            self.status = MachineStatus::ExitedErr(MachineError::EmptyGas);
            return Ok(());
        }

        let instruction = self.pc.read().unwrap();
        let available_gas = self.state.context.gas_limit - memory_cost - gas_cost;
        let result = run_opcode(instruction, &mut self.state, gas_stipend, available_gas);

        self.state.used_gas = self.state.used_gas + gas_cost;
        self.state.memory_gas = memory_cost;
        self.state.refunded_gas = self.state.refunded_gas + gas_refund;

        match result {
            None => Ok(()),
            Some(Control::Jump(dest)) => {
                self.pc.jump(dest).unwrap();
                Ok(())
            },
            Some(Control::InvokeCall(context, (from, len))) => {
                self.status = MachineStatus::InvokeCall(context, (from, len));
                Ok(())
            },
        }
    }

    pub fn status(&self) -> MachineStatus {
        self.status.clone()
    }
}
