; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_1 = alloca i32
  %merge_4 = alloca i32
  %merge_8 = alloca i32
  %t0 = add i32 0, 0
  store i32 %t0, ptr %merge_1
  %t1 = add i32 0, 1
  %t2 = add i32 0, 5
  store i32 %t1, ptr %merge_4
  br label %bb1
bb1:
  %t3 = load i32, ptr %merge_4
  %t4 = icmp sle i32 %t3, %t2
  br i1 %t4, label %bb2, label %bb4
bb2:
  %t5 = add i32 0, 1
  %t6 = add i32 0, 5
  store i32 %t5, ptr %merge_8
  br label %bb5
bb3:
  %t7 = add i32 0, 1
  %t8 = load i32, ptr %merge_4
  %t9 = add i32 %t8, %t7
  store i32 %t9, ptr %merge_4
  br label %bb1
bb4:
  %t10 = load i32, ptr %merge_1
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t10)
  ret i32 0
bb5:
  %t12 = load i32, ptr %merge_8
  %t13 = icmp sle i32 %t12, %t6
  br i1 %t13, label %bb6, label %bb8
bb6:
  %t14 = load i32, ptr %merge_1
  %t15 = load i32, ptr %merge_4
  %t16 = load i32, ptr %merge_8
  %t17 = mul i32 %t15, %t16
  %t18 = add i32 %t14, %t17
  store i32 %t18, ptr %merge_1
  br label %bb7
bb7:
  %t19 = add i32 0, 1
  %t20 = load i32, ptr %merge_8
  %t21 = add i32 %t20, %t19
  store i32 %t21, ptr %merge_8
  br label %bb5
bb8:
  br label %bb3
}

