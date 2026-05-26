use super::world_model::{
    LatentDynamics, LatentState, PredictedObservation, RewardModel, WorldModelDecoder,
};

pub struct TrajectoryStep {
    pub state: LatentState,
    pub action: String,
    pub predicted_obs: PredictedObservation,
    pub reward: f64,
}

pub struct Trajectory {
    pub steps: Vec<TrajectoryStep>,
    pub cumulative_reward: f64,
}

pub struct ImaginationRollout<'a> {
    dynamics: &'a dyn LatentDynamics,
    decoder: &'a dyn WorldModelDecoder,
    reward_model: &'a dyn RewardModel,
    horizon: usize,
    discount: f64,
}

impl<'a> ImaginationRollout<'a> {
    pub fn new(
        dynamics: &'a dyn LatentDynamics,
        decoder: &'a dyn WorldModelDecoder,
        reward_model: &'a dyn RewardModel,
        horizon: usize,
        discount: f64,
    ) -> Self {
        Self {
            dynamics,
            decoder,
            reward_model,
            horizon,
            discount: discount.clamp(0.0, 1.0),
        }
    }

    pub fn rollout(&self, initial: &LatentState, actions: &[String]) -> Trajectory {
        let mut steps = Vec::with_capacity(self.horizon);
        let mut current = initial.clone();
        let mut cumulative_reward = 0.0;
        let mut discount_factor = 1.0;

        let action_count = actions.len();
        for i in 0..self.horizon {
            let action = if action_count > 0 {
                &actions[i % action_count]
            } else {
                "noop"
            };

            let next = self.dynamics.predict(&current, action);
            let predicted_obs = self.decoder.decode(&next);
            let reward = predicted_obs.confidence;

            cumulative_reward += reward * discount_factor;
            discount_factor *= self.discount;

            steps.push(TrajectoryStep {
                state: next.clone(),
                action: action.to_string(),
                predicted_obs,
                reward,
            });

            current = next;
        }

        Trajectory {
            steps,
            cumulative_reward,
        }
    }

    pub fn evaluate_trajectory(
        &self,
        initial: &LatentState,
        actions: &[String],
        actuals: &[std::collections::HashMap<String, f64>],
    ) -> Trajectory {
        let mut steps = Vec::with_capacity(self.horizon);
        let mut current = initial.clone();
        let mut cumulative_reward = 0.0;
        let mut discount_factor = 1.0;

        let action_count = actions.len();
        for i in 0..self.horizon {
            let action = if action_count > 0 {
                &actions[i % action_count]
            } else {
                "noop"
            };

            let next = self.dynamics.predict(&current, action);
            let predicted_obs = self.decoder.decode(&next);

            let reward = if i < actuals.len() {
                self.reward_model
                    .compute_reward(&predicted_obs, &actuals[i])
            } else {
                predicted_obs.confidence
            };

            cumulative_reward += reward * discount_factor;
            discount_factor *= self.discount;

            steps.push(TrajectoryStep {
                state: next.clone(),
                action: action.to_string(),
                predicted_obs,
                reward,
            });

            current = next;
        }

        Trajectory {
            steps,
            cumulative_reward,
        }
    }

    pub fn horizon(&self) -> usize {
        self.horizon
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sce::world_model::{
        DefaultDecoder, DefaultDynamics, DefaultRewardModel, LatentState,
    };

    #[test]
    fn rollout_produces_n_steps() {
        let dynamics = DefaultDynamics { decay: 0.95 };
        let decoder = DefaultDecoder;
        let reward = DefaultRewardModel;
        let rollout = ImaginationRollout::new(&dynamics, &decoder, &reward, 5, 0.99);

        let initial = LatentState::new(vec![0.8, 0.6, 0.4]);
        let actions = vec!["explore".to_string(), "exploit".to_string()];
        let trajectory = rollout.rollout(&initial, &actions);

        assert_eq!(trajectory.steps.len(), 5);
        assert!(trajectory.cumulative_reward > 0.0);
    }

    #[test]
    fn rollout_does_not_modify_initial_state() {
        let dynamics = DefaultDynamics { decay: 0.95 };
        let decoder = DefaultDecoder;
        let reward = DefaultRewardModel;
        let rollout = ImaginationRollout::new(&dynamics, &decoder, &reward, 3, 0.99);

        let initial = LatentState::new(vec![1.0, 0.5]);
        let actions = vec!["act".to_string()];
        let _trajectory = rollout.rollout(&initial, &actions);

        assert!((initial.dims[0] - 1.0).abs() < f64::EPSILON);
        assert!((initial.dims[1] - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn evaluate_trajectory_with_actuals() {
        let dynamics = DefaultDynamics { decay: 0.95 };
        let decoder = DefaultDecoder;
        let reward_model = DefaultRewardModel;
        let rollout = ImaginationRollout::new(&dynamics, &decoder, &reward_model, 3, 0.99);

        let initial = LatentState::new(vec![0.5, 0.5]);
        let actions = vec!["act".to_string()];
        let mut actual_step = std::collections::HashMap::new();
        actual_step.insert("dim_0".to_string(), 0.475);
        actual_step.insert("dim_1".to_string(), 0.475);
        let actuals = vec![actual_step];

        let trajectory = rollout.evaluate_trajectory(&initial, &actions, &actuals);
        assert_eq!(trajectory.steps.len(), 3);
        assert!(trajectory.steps[0].reward > 0.5);
    }

    #[test]
    fn rollout_with_empty_actions_uses_noop() {
        let dynamics = DefaultDynamics { decay: 0.95 };
        let decoder = DefaultDecoder;
        let reward = DefaultRewardModel;
        let rollout = ImaginationRollout::new(&dynamics, &decoder, &reward, 2, 0.99);

        let initial = LatentState::new(vec![0.5]);
        let trajectory = rollout.rollout(&initial, &[]);

        assert_eq!(trajectory.steps.len(), 2);
        assert_eq!(trajectory.steps[0].action, "noop");
    }

    #[test]
    fn discount_reduces_future_rewards() {
        let dynamics = DefaultDynamics { decay: 1.0 };
        let decoder = DefaultDecoder;
        let reward = DefaultRewardModel;

        let high_discount = ImaginationRollout::new(&dynamics, &decoder, &reward, 5, 0.5);
        let no_discount = ImaginationRollout::new(&dynamics, &decoder, &reward, 5, 1.0);

        let initial = LatentState::new(vec![0.8]);
        let actions = vec!["act".to_string()];

        let t_high = high_discount.rollout(&initial, &actions);
        let t_none = no_discount.rollout(&initial, &actions);

        assert!(t_high.cumulative_reward < t_none.cumulative_reward);
    }
}
